//! Live Editor's WebAssembly module. It links the View IR the MicroTS
//! compiler emits into BVIR exactly as `ui-link` does, so a view built in the
//! browser is byte for byte the one the toolchain builds, and it runs the
//! linked view in a [`preview::Preview`], drawn by the device's own UI core.
//!
//! The web app reaches it through a small ABI. `input(len)` sizes the input
//! buffer and answers its address, and the caller writes its bytes there;
//! each call then reads that input, and `output()` and `output_len()` locate
//! its result. Calls that can fail answer 0 on success and 1 with the error's
//! text as output.
//!
//! | Export | Input | Answer and output |
//! | --- | --- | --- |
//! | `link()` | View IR JSON | 0 with the BVIR |
//! | `stage(kind)` | a view file | keeps it for `open`: 0 the BVIR, 1 the stylesheet, 2 the view's font atlas, 3 a system font atlas |
//! | `open(width, height)` | | 0 once the staged view runs; the view's files are then unstaged, the system fonts kept |
//! | `contract()` | | the view's values and handlers as JSON |
//! | `set()` | `{"name", "value"}` JSON | 0 once written, 1 with `view:set`'s error |
//! | `frame()` | | steps one tick |
//! | `tick_rate()` | | ticks per second |
//! | `draw(scale)` | | 1 with new RGBA pixels, 0 when the picture did not change |
//! | `press(x, y)` | | 1 with the handler call as JSON, 0 when the tap hits no handler |
//! | `texts()` | | the text on screen as a JSON array |

pub mod preview;

use barracuda_ui_runtime::{compiler, format};

/// Links MicroTS View IR JSON into BVIR bytes, or says why it cannot.
///
/// # Errors
///
/// Returns the linker's reason, in words, when the input is not View IR the
/// device's UI runtime can run.
pub fn link_view(json: &[u8]) -> Result<Vec<u8>, String> {
    match compiler::link(json) {
        Ok(program) => Ok(format::write(&program)),
        Err(error) => Err(describe(&error)),
    }
}

fn describe(error: &compiler::LinkError) -> String {
    match error {
        compiler::LinkError::Json => String::from("the view is not View IR JSON"),
        compiler::LinkError::Version => String::from("the View IR version is not 1"),
        compiler::LinkError::Unsupported(what) => format!("views do not support {what} yet"),
        compiler::LinkError::Unknown(what) => format!("the view uses an undeclared {what}"),
        compiler::LinkError::TooManyNodes => String::from("the view has too many nodes"),
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod exports {
    //! The WebAssembly ABI. Every export is safe Rust; the attribute that
    //! keeps their names is the only unsafe part.

    use core::cell::RefCell;

    use super::preview::Preview;

    /// The files `stage` keeps for the next `open`.
    #[derive(Default)]
    struct Staged {
        program: Vec<u8>,
        styles: Vec<u8>,
        fonts: Vec<Vec<u8>>,
        system: Vec<Vec<u8>>,
    }

    std::thread_local! {
        static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static STAGED: RefCell<Staged> = RefCell::new(Staged::default());
        static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
    }

    fn answer(bytes: Vec<u8>) {
        OUTPUT.with(|output| *output.borrow_mut() = bytes);
    }

    fn fail(reason: String) -> u32 {
        answer(reason.into_bytes());
        1
    }

    fn with_preview<T>(run: impl FnOnce(&mut Preview) -> T) -> Option<T> {
        PREVIEW.with(|preview| preview.borrow_mut().as_mut().map(run))
    }

    /// Makes the input buffer `len` bytes long and answers its address.
    #[unsafe(no_mangle)]
    pub extern "C" fn input(len: usize) -> *mut u8 {
        INPUT.with(|input| {
            let mut input = input.borrow_mut();
            input.clear();
            input.resize(len, 0);
            input.as_mut_ptr()
        })
    }

    /// Links the input: answers 0 with the BVIR as output, or 1 with the
    /// error's text as output.
    #[unsafe(no_mangle)]
    pub extern "C" fn link() -> u32 {
        let (bytes, status) = INPUT.with(|input| match super::link_view(&input.borrow()) {
            Ok(bytes) => (bytes, 0),
            Err(reason) => (reason.into_bytes(), 1),
        });
        answer(bytes);
        status
    }

    /// Keeps the input as one of the view's files for the next `open`.
    #[unsafe(no_mangle)]
    pub extern "C" fn stage(kind: u32) {
        let bytes = INPUT.with(|input| core::mem::take(&mut *input.borrow_mut()));
        STAGED.with(|staged| {
            let mut staged = staged.borrow_mut();
            match kind {
                0 => staged.program = bytes,
                1 => staged.styles = bytes,
                2 => staged.fonts.push(bytes),
                _ => staged.system.push(bytes),
            }
        });
    }

    /// Mounts the staged view at `width` × `height`, replacing the running one.
    #[unsafe(no_mangle)]
    pub extern "C" fn open(width: u32, height: u32) -> u32 {
        let opened = STAGED.with(|staged| {
            let mut staged = staged.borrow_mut();
            let own: Vec<&[u8]> = staged.fonts.iter().map(Vec::as_slice).collect();
            let system: Vec<&[u8]> = staged.system.iter().map(Vec::as_slice).collect();
            let opened = Preview::open(
                &staged.program,
                &staged.styles,
                &own,
                &system,
                width,
                height,
            );
            staged.program.clear();
            staged.styles.clear();
            staged.fonts.clear();
            opened
        });
        match opened {
            Ok(preview) => {
                PREVIEW.with(|slot| *slot.borrow_mut() = Some(preview));
                0
            }
            Err(reason) => {
                PREVIEW.with(|slot| *slot.borrow_mut() = None);
                fail(reason)
            }
        }
    }

    /// Answers the running view's contract as JSON.
    #[unsafe(no_mangle)]
    pub extern "C" fn contract() {
        let json = with_preview(|preview| preview.contract().to_string()).unwrap_or_default();
        answer(json.into_bytes());
    }

    /// Writes one view value from `{"name", "value"}` JSON.
    #[unsafe(no_mangle)]
    pub extern "C" fn set() -> u32 {
        let call = INPUT.with(|input| serde_json::from_slice::<serde_json::Value>(&input.borrow()));
        let Ok(call) = call else {
            return fail(String::from("the value is not JSON"));
        };
        let name = call
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("");
        let value = call.get("value").unwrap_or(&serde_json::Value::Null);
        match with_preview(|preview| preview.set(name, value)) {
            Some(Ok(())) => 0,
            Some(Err(reason)) => fail(reason),
            None => fail(String::from("the view is closed")),
        }
    }

    /// Steps the running view one tick.
    #[unsafe(no_mangle)]
    pub extern "C" fn frame() {
        with_preview(Preview::frame);
    }

    /// Ticks per second of the running view, 0 without one.
    #[unsafe(no_mangle)]
    pub extern "C" fn tick_rate() -> u32 {
        with_preview(|preview| preview.tick_rate()).unwrap_or(0)
    }

    /// Draws the running view: 1 with new RGBA pixels as output, 0 when the
    /// picture is the one drawn last.
    #[unsafe(no_mangle)]
    pub extern "C" fn draw(scale: u32) -> u32 {
        let pixels = with_preview(|preview| preview.draw(scale).map(<[u8]>::to_vec)).flatten();
        match pixels {
            Some(pixels) => {
                answer(pixels);
                1
            }
            None => 0,
        }
    }

    /// Taps (`x`, `y`): 1 with the handler call as JSON, 0 when no handler is there.
    #[unsafe(no_mangle)]
    pub extern "C" fn press(x: f32, y: f32) -> u32 {
        match with_preview(|preview| preview.press(x, y)).flatten() {
            Some(call) => {
                answer(call.to_string().into_bytes());
                1
            }
            None => 0,
        }
    }

    /// Answers the text on screen as a JSON array.
    #[unsafe(no_mangle)]
    pub extern "C" fn texts() {
        let texts = with_preview(|preview| preview.texts()).unwrap_or_default();
        answer(serde_json::Value::from(texts).to_string().into_bytes());
    }

    /// The last output's address.
    #[unsafe(no_mangle)]
    pub extern "C" fn output() -> *const u8 {
        OUTPUT.with(|output| output.borrow().as_ptr())
    }

    /// The last output's length.
    #[unsafe(no_mangle)]
    pub extern "C" fn output_len() -> usize {
        OUTPUT.with(|output| output.borrow().len())
    }
}

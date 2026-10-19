//! Live Editor's compiler, run in the browser as WebAssembly. It links the
//! View IR the MicroTS compiler emits into BVIR exactly as `ui-link` does, so
//! a view built in the browser is byte for byte the one the toolchain builds.
//!
//! The web app reaches it through four exports: `input(len)` sizes the input
//! buffer and answers its address, the caller writes the View IR JSON there,
//! `link()` links it, and `output()` and `output_len()` locate the result,
//! which is the BVIR on success and the error's text on failure.

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

    std::thread_local! {
        static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
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
        OUTPUT.with(|output| *output.borrow_mut() = bytes);
        status
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

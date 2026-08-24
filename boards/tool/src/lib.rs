//! Board selection command support.

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use barracuda_board_config::{
    parse, read_selected_board, validate_board_name, write_selected_board, ConfigError,
    SelectionError,
};
use dialoguer::{theme::ColorfulTheme, FuzzySelect};

const USAGE: &str = "usage: cargo board select [board-name]";

/// Failure while selecting a Board bundle.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// Arguments do not match the single supported command.
    #[error("{USAGE}")]
    Usage,
    /// The requested Board bundle does not exist.
    #[error("Board `{name}` does not exist under `boards/configs`")]
    BoardMissing {
        /// Requested Board name.
        name: String,
    },
    /// The bundle directory does not agree with its YAML identity.
    #[error("Board directory `{directory}` declares Board `{declared}`")]
    NameMismatch {
        /// Bundle directory name.
        directory: String,
        /// Name declared by `board.yml`.
        declared: String,
    },
    /// The Board bundle does not contain its declared native layout.
    #[error("Board `{name}` native layout `{path}` does not exist")]
    NativeLayoutMissing {
        /// Selected Board name.
        name: String,
        /// Missing native-layout path.
        path: PathBuf,
    },
    /// A Board configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Persistent selection state is invalid or unavailable.
    #[error(transparent)]
    Selection(#[from] SelectionError),
    /// A Board bundle could not be read.
    #[error("failed to read Board bundle `{path}`: {source}")]
    Read {
        /// File that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The Board catalog directory could not be read.
    #[error("failed to read Board catalog `{path}`: {source}")]
    Catalog {
        /// Directory or entry that could not be read.
        path: PathBuf,
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The Board catalog contains no selectable bundles.
    #[error("no Board bundles found under `boards/configs`")]
    EmptyCatalog,
    /// The terminal prompt failed.
    #[error("interactive Board selection failed: {0}")]
    Prompt(#[source] dialoguer::Error),
    /// An interactive selector returned an invalid catalog position.
    #[error("interactive Board selection returned invalid index {index}")]
    InvalidSelectionIndex {
        /// Invalid index returned by the selector.
        index: usize,
    },
    /// Command output could not be written.
    #[error("failed to write command output: {0}")]
    Output(#[source] io::Error),
}

/// Runs the workspace Board command against `workspace_root`.
///
/// # Errors
///
/// Returns [`CommandError`] when the command syntax is invalid, the requested
/// Board bundle is incomplete, or the selection cannot be persisted.
pub fn run<I, S, W>(args: I, workspace_root: &Path, output: &mut W) -> Result<(), CommandError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    W: Write,
{
    run_with_selector(args, workspace_root, output, prompt_for_board)
}

fn run_with_selector<I, S, W, F>(
    args: I,
    workspace_root: &Path,
    output: &mut W,
    selector: F,
) -> Result<(), CommandError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
    W: Write,
    F: FnOnce(&[String], Option<usize>) -> Result<Option<usize>, CommandError>,
{
    let args = args
        .into_iter()
        .map(|argument| argument.as_ref().to_owned())
        .collect::<Vec<_>>();
    match args.as_slice() {
        [command] if command == "select" => {
            let boards = discover_boards(workspace_root)?;
            let current = read_selected_board(workspace_root)?;
            let default = current
                .as_ref()
                .and_then(|current| boards.iter().position(|board| board == current));
            let Some(index) = selector(&boards, default)? else {
                writeln!(output, "Board selection cancelled.").map_err(CommandError::Output)?;
                return Ok(());
            };
            let name = boards
                .get(index)
                .ok_or(CommandError::InvalidSelectionIndex { index })?;
            select_board(workspace_root, name, output)
        }
        [command, name] if command == "select" => select_board(workspace_root, name, output),
        _ => Err(CommandError::Usage),
    }
}

fn prompt_for_board(
    boards: &[String],
    default: Option<usize>,
) -> Result<Option<usize>, CommandError> {
    let theme = ColorfulTheme::default();
    FuzzySelect::with_theme(&theme)
        .with_prompt("Select a Board — type to search")
        .default(default.unwrap_or(0))
        .items(boards)
        .interact_opt()
        .map_err(CommandError::Prompt)
}

fn discover_boards(workspace_root: &Path) -> Result<Vec<String>, CommandError> {
    let catalog = workspace_root.join("boards/configs");
    let entries = fs::read_dir(&catalog).map_err(|source| CommandError::Catalog {
        path: catalog.clone(),
        source,
    })?;
    let mut boards = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CommandError::Catalog {
            path: catalog.clone(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| CommandError::Catalog {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if validate_board_name(&name).is_ok() {
            boards.push(name);
        }
    }
    boards.sort_unstable();
    if boards.is_empty() {
        Err(CommandError::EmptyCatalog)
    } else {
        Ok(boards)
    }
}

fn select_board<W: Write>(
    workspace_root: &Path,
    name: &str,
    output: &mut W,
) -> Result<(), CommandError> {
    validate_board_name(name)?;
    let bundle = workspace_root.join("boards/configs").join(name);
    let board_path = bundle.join("board.yml");
    let yaml = match fs::read_to_string(&board_path) {
        Ok(yaml) => yaml,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(CommandError::BoardMissing {
                name: name.to_owned(),
            });
        }
        Err(source) => {
            return Err(CommandError::Read {
                path: board_path,
                source,
            });
        }
    };
    let board = parse(&yaml)?;
    if board.name() != name {
        return Err(CommandError::NameMismatch {
            directory: name.to_owned(),
            declared: board.name().to_owned(),
        });
    }
    let native_layout = bundle.join(board.native_layout().artifact());
    if !native_layout.is_file() {
        return Err(CommandError::NativeLayoutMissing {
            name: name.to_owned(),
            path: native_layout,
        });
    }

    write_selected_board(workspace_root, name)?;
    writeln!(output, "Selected Board `{name}`.").map_err(CommandError::Output)?;
    writeln!(output, "Run `cargo build` to build it.").map_err(CommandError::Output)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs, path::Path};

    use barracuda_board_config::{read_selected_board, write_selected_board};
    use tempfile::tempdir;

    use super::run_with_selector;

    fn add_board(root: &Path, name: &str) {
        let directory = root.join("boards/configs").join(name);
        fs::create_dir_all(&directory).expect("Board directory");
        fs::write(
            directory.join("board.yml"),
            format!(
                "name: {name}\nhardware:\n  chip: test\nnative-layout:\n  artifact: layout.yml\n"
            ),
        )
        .expect("Board YAML");
        fs::write(directory.join("layout.yml"), "layout\n").expect("native layout");
    }

    #[test]
    fn interactive_select_lists_boards_in_name_order_and_persists_the_choice() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "stm32f429zi-nucleo");
        add_board(root.path(), "esp32c6-devkitc-1");
        fs::write(root.path().join("boards/configs/README.txt"), "not a Board")
            .expect("non-Board file");
        let mut output = Vec::new();

        run_with_selector(["select"], root.path(), &mut output, |boards, default| {
            assert_eq!(boards, ["esp32c6-devkitc-1", "stm32f429zi-nucleo"]);
            assert_eq!(default, None);
            Ok(Some(1))
        })
        .expect("interactive selection");

        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            Some(String::from("stm32f429zi-nucleo"))
        );
    }

    #[test]
    fn interactive_select_highlights_the_current_board_by_default() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "local-linux");
        add_board(root.path(), "local-macos");
        write_selected_board(root.path(), "local-macos").expect("current Board");

        run_with_selector(
            ["select"],
            root.path(),
            &mut Vec::new(),
            |boards, default| {
                assert_eq!(boards, ["local-linux", "local-macos"]);
                assert_eq!(default, Some(1));
                Ok(Some(0))
            },
        )
        .expect("interactive selection");

        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            Some(String::from("local-linux"))
        );
    }

    #[test]
    fn interactive_select_rejects_an_empty_board_catalog() {
        let root = tempdir().expect("temporary workspace");
        fs::create_dir_all(root.path().join("boards/configs")).expect("configs directory");
        let prompted = Cell::new(false);

        let error = run_with_selector(
            ["select"],
            root.path(),
            &mut Vec::new(),
            |_boards, _default| {
                prompted.set(true);
                Ok(Some(0))
            },
        )
        .expect_err("empty Board catalog");

        assert!(!prompted.get());
        assert!(error.to_string().contains("no Board bundles found"));
    }

    #[test]
    fn explicit_board_name_does_not_open_the_interactive_prompt() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "local-macos");
        let prompted = Cell::new(false);

        run_with_selector(
            ["select", "local-macos"],
            root.path(),
            &mut Vec::new(),
            |_boards, _default| {
                prompted.set(true);
                Ok(Some(0))
            },
        )
        .expect("explicit selection");

        assert!(!prompted.get());
    }

    #[test]
    fn cancelling_interactive_select_preserves_the_current_board() {
        let root = tempdir().expect("temporary workspace");
        add_board(root.path(), "local-linux");
        add_board(root.path(), "local-macos");
        write_selected_board(root.path(), "local-macos").expect("current Board");
        let mut output = Vec::new();

        run_with_selector(["select"], root.path(), &mut output, |_boards, _default| {
            Ok(None)
        })
        .expect("cancel selection");

        assert_eq!(
            read_selected_board(root.path()).expect("read selection"),
            Some(String::from("local-macos"))
        );
        assert_eq!(
            String::from_utf8(output).expect("UTF-8 output"),
            "Board selection cancelled.\n"
        );
    }
}

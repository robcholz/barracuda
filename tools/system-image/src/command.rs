//! Shell-free execution of Platform-owned command drivers.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use barracuda_platform_config::{CommandDriver, ReleaseCommand};

pub(crate) struct DriverContext<'a> {
    pub(crate) workspace: &'a Path,
    pub(crate) platform: &'a Path,
    pub(crate) layout: &'a Path,
    pub(crate) chip: &'a str,
    pub(crate) image: Option<&'a Path>,
    pub(crate) offset: Option<u64>,
    pub(crate) size: Option<usize>,
    pub(crate) release: Option<ReleaseContext<'a>>,
}

/// The inputs a Platform's release command reads and the file it writes.
pub(crate) struct ReleaseContext<'a> {
    pub(crate) application: &'a Path,
    pub(crate) output: &'a Path,
    pub(crate) flash_size: Option<&'a str>,
}

pub(crate) struct PreparedCommand {
    pub(crate) program: PathBuf,
    pub(crate) arguments: Vec<OsString>,
}

pub(crate) fn prepare(driver: &CommandDriver, context: DriverContext<'_>) -> PreparedCommand {
    prepare_program(driver.program(), driver.arguments(), &context)
}

/// Prepares a release command, which must find every value it names.
pub(crate) fn prepare_release(
    command: &ReleaseCommand,
    context: DriverContext<'_>,
) -> Result<PreparedCommand, String> {
    let prepared = prepare_program(command.program(), command.arguments(), &context);
    if let Some(argument) = prepared
        .arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .find(|argument| argument.contains("{flash-size}"))
    {
        return Err(format!(
            "release argument `{argument}` needs the Board's flash size; declare `hardware.flash-size` in its board.yml"
        ));
    }
    Ok(prepared)
}

fn prepare_program(
    program: &Path,
    arguments: &[String],
    context: &DriverContext<'_>,
) -> PreparedCommand {
    let program = resolve_program(program, context.platform);
    let arguments = arguments
        .iter()
        .map(|argument| OsString::from(expand(argument, context)))
        .collect();
    PreparedCommand { program, arguments }
}

pub(crate) fn output(command: &PreparedCommand) -> Result<Vec<u8>, String> {
    let rendered = render(command);
    let output = Command::new(&command.program)
        .args(&command.arguments)
        .output()
        .map_err(|error| format!("failed to run `{rendered}`: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "`{rendered}` exited with {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(crate) fn status(command: &PreparedCommand) -> Result<(), String> {
    let rendered = render(command);
    let status = Command::new(&command.program)
        .args(&command.arguments)
        .status()
        .map_err(|error| format!("failed to run `{rendered}`: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{rendered}` exited with {status}"))
    }
}

pub(crate) fn render(command: &PreparedCommand) -> String {
    std::iter::once(command.program.as_os_str().to_owned())
        .chain(command.arguments.iter().cloned())
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_program(program: &Path, platform: &Path) -> PathBuf {
    if program.is_absolute()
        || matches!(
            program.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
    {
        program.to_path_buf()
    } else {
        platform.join(program)
    }
}

fn expand(template: &str, context: &DriverContext<'_>) -> String {
    let mut expanded = template
        .replace("{workspace}", &context.workspace.display().to_string())
        .replace("{platform}", &context.platform.display().to_string())
        .replace("{layout}", &context.layout.display().to_string())
        .replace("{chip}", context.chip);
    if let Some(image) = context.image {
        expanded = expanded.replace("{image}", &image.display().to_string());
    }
    if let Some(offset) = context.offset {
        expanded = expanded.replace("{offset}", &format!("{offset:#x}"));
    }
    if let Some(size) = context.size {
        expanded = expanded.replace("{size}", &size.to_string());
    }
    if let Some(release) = &context.release {
        expanded = expanded
            .replace("{application}", &release.application.display().to_string())
            .replace("{output}", &release.output.display().to_string());
        if let Some(flash_size) = release.flash_size {
            expanded = expanded.replace("{flash-size}", flash_size);
        }
    }
    expanded
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::path::Path;

    use barracuda_platform_config::CommandDriver;

    #[test]
    fn expands_a_platform_command_without_a_shell() {
        let yaml = "program: flash-tool\narguments: [write, '{chip}', '{offset}', '{image}']\n";
        let driver = yaml_peg::serde::from_str::<CommandDriver>(yaml)
            .expect("command YAML")
            .pop()
            .expect("command document");
        let command = super::prepare(
            &driver,
            super::DriverContext {
                workspace: Path::new("/workspace"),
                platform: Path::new("/workspace/platforms/acme"),
                layout: Path::new("/workspace/boards/configs/acme/layout"),
                chip: "acme-1",
                image: Some(Path::new("/workspace/system.img")),
                offset: Some(0x4000),
                size: Some(8192),
                release: None,
            },
        );

        assert_eq!(command.program, Path::new("flash-tool"));
        assert_eq!(
            command.arguments,
            ["write", "acme-1", "0x4000", "/workspace/system.img"]
        );
    }
}

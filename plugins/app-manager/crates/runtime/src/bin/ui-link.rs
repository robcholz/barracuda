//! Links MicroTS View IR JSON into a BVIR file: `ui-link <view.json> <view.bvir>`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("usage: ui-link <view.json> <view.bvir>");
        return ExitCode::FAILURE;
    };
    let json = match std::fs::read(input) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("{input}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let program = match barracuda_ui_runtime::compiler::link(&json) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("{input}: {error:?}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = std::fs::write(output, barracuda_ui_runtime::format::write(&program)) {
        eprintln!("{output}: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

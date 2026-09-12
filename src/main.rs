use std::process::ExitCode;

fn main() -> ExitCode {
    match gbd::run() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

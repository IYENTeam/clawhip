use std::env;
use std::path::PathBuf;
use std::process::Command;

fn op_pi_path() -> Result<PathBuf, String> {
    let executable =
        env::current_exe().map_err(|error| format!("resolve clawhip path: {error}"))?;
    Ok(executable.with_file_name(format!("op-pi{}", env::consts::EXE_SUFFIX)))
}

#[cfg(unix)]
fn main() {
    use std::os::unix::process::CommandExt;

    let op_pi = op_pi_path().unwrap_or_else(|error| {
        eprintln!("clawhip compatibility wrapper: {error}");
        std::process::exit(1);
    });
    let error = Command::new(op_pi).args(env::args_os().skip(1)).exec();
    eprintln!("clawhip compatibility wrapper: failed to start op-pi: {error}");
    std::process::exit(1);
}

#[cfg(not(unix))]
fn main() {
    let op_pi = op_pi_path().unwrap_or_else(|error| {
        eprintln!("clawhip compatibility wrapper: {error}");
        std::process::exit(1);
    });
    let status = Command::new(op_pi)
        .args(env::args_os().skip(1))
        .status()
        .unwrap_or_else(|error| {
            eprintln!("clawhip compatibility wrapper: failed to start op-pi: {error}");
            std::process::exit(1);
        });
    std::process::exit(status.code().unwrap_or(1));
}

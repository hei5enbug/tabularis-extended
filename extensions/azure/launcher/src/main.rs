fn main() {
    use tabularis_azure_launcher::{launch, LaunchError, Layout};
    let result = if std::env::args_os().nth(1).is_some() {
        Err(LaunchError::InvalidArguments)
    } else {
        Layout::discover().and_then(|layout| launch(&layout))
    };
    match result {
        Ok(code) => std::process::exit(code as i32),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

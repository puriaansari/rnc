mod cli;
mod net;

use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = std::env::args().skip(1);

    match cli::parse_args(args) {
        Ok(cli::Command::Help) => {
            print!("{}", cli::help());
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Version) => {
            println!("rnc {}", cli::VERSION);
            ExitCode::SUCCESS
        }
        Ok(cli::Command::Run(config)) => match net::run(config) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                if let Some(io_error) = error.downcast_ref::<io::Error>()
                    && io_error.kind() == io::ErrorKind::BrokenPipe
                {
                    return ExitCode::SUCCESS;
                }

                eprintln!("rnc: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("rnc: {error}");
            eprintln!("{}", cli::USAGE);
            eprintln!("Try 'rnc --help' for more information.");
            ExitCode::FAILURE
        }
    }
}

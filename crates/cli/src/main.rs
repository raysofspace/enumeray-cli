mod analysis;
mod app;
mod args;
mod candidate;
mod collections;
mod commands;
mod config;
mod manifest;
mod observation;
mod output;
mod preflight;
mod release;
mod session;
mod starter;
mod transfers;

use args::Args;
use clap::{CommandFactory, Parser, Subcommand};
use enumeray_client::Error;
use serde_json::json;
use std::io::Write;

fn main() -> std::process::ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error) => {
            // Preserve the host parser's non-reflecting diagnostics: malformed
            // invocations can accidentally contain a credential as an argument.
            // Invalid-value errors may omit usage context. Ask the same parser
            // for partial matches solely to identify the credential domain.
            let private_command = error.use_stderr()
                && Args::command()
                    .ignore_errors(true)
                    .try_get_matches()
                    .is_ok_and(|matches| match matches.subcommand() {
                        Some(("connector", child)) => child
                            .subcommand_name()
                            .is_none_or(enumeray_connector::HostCommand::has_subcommand),
                        Some((name, _)) => enumeray_connector::RuntimeCommand::has_subcommand(name),
                        None => false,
                    });
            if error.use_stderr() && private_command {
                eprintln!("enumeray: invalid connector or runner command; use --help for usage");
                return std::process::ExitCode::from(2);
            }
            error.exit()
        }
    };
    // Select the credential domain before any profile access or client setup.
    let args = match args {
        Args {
            command: args::Command::Runtime(command),
            ..
        } => {
            return enumeray_connector::execute_runtime(command);
        }
        Args {
            command: args::Command::Connector(args::Connector::Host(command)),
            ..
        } => {
            return enumeray_connector::execute_host(command);
        }
        args => args,
    };
    let result = app::execute(&args);
    match result {
        Ok(completion) => {
            if let Some(value) = completion.value {
                let mut stdout = std::io::stdout().lock();
                let text = if args.json {
                    serde_json::to_string(&value).expect("JSON value")
                } else {
                    output::render(&args, &value)
                };
                if writeln!(stdout, "{text}")
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    return std::process::ExitCode::FAILURE;
                }
            }
            completion.exit.into()
        }
        Err(failure) => {
            let error = failure.error;
            if args.json {
                let value = match &error {
                    Error::Api {
                        status,
                        failure,
                        retry_after,
                    } => json!({"status":status,"error":failure,"retry_after":retry_after}),
                    Error::Session { code, message } => {
                        json!({"error":{"code":code.as_str(),"message":message}})
                    }
                    Error::OAuth {
                        status,
                        kind,
                        retry_after,
                    } => {
                        json!({"status":status,"error":{"code":kind.as_str(),"message":error.to_string()},"retry_after":retry_after})
                    }
                    _ => json!({"error":{"code":"client_error","message":error.to_string()}}),
                };
                eprintln!("{value}");
            } else {
                eprintln!("enumeray: {}", output::safe_text(&error.to_string(), true));
                if let Some(hint) = output::error_hint(&args, &error) {
                    eprintln!("{hint}");
                }
            }
            failure.exit.into()
        }
    }
}

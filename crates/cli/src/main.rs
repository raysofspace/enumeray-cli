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
mod setup;
mod starter;
mod transfers;

use args::Args;
use clap::{CommandFactory, Parser, Subcommand};
use enumeray_client::Error;
use enumeray_contracts::automation::{CliErrorDetail, CliRuntimeError, SchemaVersion};
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
                        Some(("setup", child)) => child.subcommand_name() == Some("apply-host"),
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
                let (status, code, message, request_id, details, retry_after) = match &error {
                    Error::Api {
                        status,
                        failure,
                        retry_after,
                    } => (
                        Some(*status),
                        failure.code.clone(),
                        failure.message.clone(),
                        Some(failure.request_id.clone()),
                        failure.details.clone(),
                        retry_after.clone(),
                    ),
                    Error::Session { code, message } => (
                        None,
                        code.as_str().into(),
                        message.clone(),
                        None,
                        None,
                        None,
                    ),
                    Error::OAuth {
                        status,
                        kind,
                        retry_after,
                    } => (
                        Some(*status),
                        kind.as_str().into(),
                        error.to_string(),
                        None,
                        None,
                        retry_after.clone(),
                    ),
                    _ => (
                        None,
                        "client_error".into(),
                        error.to_string(),
                        None,
                        None,
                        None,
                    ),
                };
                let value = CliRuntimeError {
                    schema_version: SchemaVersion,
                    status,
                    error: CliErrorDetail {
                        code,
                        message,
                        request_id,
                        details,
                    },
                    retry_after,
                };
                eprintln!(
                    "{}",
                    serde_json::to_string(&value).expect("runtime error JSON")
                );
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

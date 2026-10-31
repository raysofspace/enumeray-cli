//! Application dispatch; workflow modules own individual commands.

use crate::{
    args::{Args, Command, Model},
    commands, config,
    manifest::publication_source,
    starter,
};
use enumeray_client::Error;
use reqwest::Method;
use serde_json::{Value, json};

fn execute_value(args: &Args) -> Result<Value, Error> {
    if let Command::Model(Model::Init {
        id,
        directory,
        kind,
        io,
        driver,
    }) = &args.command
    {
        return starter::init(id, directory, *kind, *io, *driver);
    }
    if let Command::Model(Model::Validate { file }) = &args.command {
        let (_, source) = publication_source(file)?;
        source
            .candidate("validation-only")
            .map_err(|error| Error::Invalid(error.to_string()))?;
        return Ok(
            json!({"valid":true,"model_id":source.model.id,"complete":true,"validation":"local_contract","authorization_validation":"requires_api","context_validation":"requires_api","installation_validation":"requires_connector"}),
        );
    }
    let config_path = args
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(config::default_path)?;
    if let Command::Auth(command) = &args.command {
        return commands::auth(args, command, &config_path);
    }
    let profiles = config::load(&config_path)?;
    let client = config::client(args, &profiles)?;
    match &args.command {
        Command::Setup(_) => unreachable!("setup dispatch"),
        Command::Auth(_) => unreachable!(),
        Command::Runtime(_) => unreachable!("runtime dispatch precedes user profile access"),
        Command::Service(command) => commands::service(&client, command),
        Command::Connector(command) => commands::connector(&client, command, args.json),
        Command::Model(command) => commands::model(&client, command),
        Command::Access(command) => commands::access(&client, command, args.json),
        Command::Run(command) => commands::run(&client, command),
        Command::Artifact(command) => commands::artifact(&client, command),
        Command::Collection(command) => crate::collections::execute(&client, command),
        Command::Analysis(_) => unreachable!("analysis dispatch"),
        Command::Api {
            method,
            path,
            file,
            idempotency_key,
        } => {
            let body: Option<Value> = file
                .as_ref()
                .map(|file| config::read_json(file))
                .transpose()?;
            client.request(
                Method::from_bytes(method.as_bytes())
                    .map_err(|_| Error::Invalid("Invalid method.".into()))?,
                path,
                body.as_ref(),
                idempotency_key.as_deref(),
            )
        }
    }
}

/// Process completion is deliberately separate from API response values.
pub(crate) struct Completion {
    pub value: Option<Value>,
    pub exit: u8,
}

pub(crate) struct Failure {
    pub error: Error,
    pub exit: u8,
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self { error, exit: 1 }
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Error::from(error).into()
    }
}

pub(crate) fn execute(args: &Args) -> Result<Completion, Failure> {
    if let Command::Setup(command) = &args.command {
        return crate::setup::execute(args, command);
    }
    if let Command::Connector(crate::args::Connector::Context(
        crate::args::ConnectorContext::Preflight {
            connector,
            context,
            directory,
            interval,
            deadline,
        },
    )) = &args.command
    {
        let path = args
            .config
            .clone()
            .map(Ok)
            .unwrap_or_else(config::default_path)?;
        let profiles = config::load(&path)?;
        let client = config::client(args, &profiles)?;
        return crate::preflight::execute(
            &client, connector, context, directory, *interval, *deadline,
        );
    }
    if let Command::Analysis(crate::args::Analysis::Import {
        prepared,
        directory,
    }) = &args.command
    {
        return crate::analysis::recovery::import(args, prepared, directory)
            .map(|value| Completion {
                value: Some(value),
                exit: 0,
            })
            .map_err(Into::into);
    }
    if let Command::Model(Model::Release(command)) = &args.command {
        let path = args
            .config
            .clone()
            .map(Ok)
            .unwrap_or_else(config::default_path)?;
        let profiles = config::load(&path)?;
        let client = config::client(args, &profiles)?;
        return crate::release::execute(args, &client, command);
    }
    if let Command::Analysis(command) = &args.command {
        let path = args
            .config
            .clone()
            .map(Ok)
            .unwrap_or_else(config::default_path)?;
        let profiles = config::load(&path)?;
        let client = config::client(args, &profiles)?;
        return crate::analysis::execute(args, &client, command);
    }
    if matches!(
        &args.command,
        Command::Run(crate::args::Run::Wait { .. } | crate::args::Run::Logs { follow: true, .. })
    ) {
        let path = args
            .config
            .clone()
            .map(Ok)
            .unwrap_or_else(config::default_path)?;
        let profiles = config::load(&path)?;
        let client = config::client(args, &profiles)?;
        return crate::observation::execute(args, &client);
    }
    execute_value(args)
        .map(|value| Completion {
            value: Some(value),
            exit: 0,
        })
        .map_err(Into::into)
}

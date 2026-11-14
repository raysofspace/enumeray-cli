//! Author workflow. The release and analysis journals own all remote operations.
use crate::{
    analysis,
    app::{Completion, Failure},
    args::{
        Analysis, Args, ModelSetup, ModelSetupCommand, Release, StarterDriver, StarterIo,
        StarterKind,
    },
    config, manifest,
    output::{invocation, safe_text},
    release, starter,
};
use enumeray_client::{EnumerayClient, Error};
use enumeray_contracts::{
    collections,
    management::{AvailableGrant, ModelCreate},
    models::{ParsedPublication, ScientificExample, files},
    numeric::equivalent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, BufRead, IsTerminal, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const STATE: &str = "model-setup.json";
const VERSION: u32 = 1;

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    directory: PathBuf,
    project: Option<PathBuf>,
    local_check: Option<String>,
    reference: Option<Reference>,
    account: Option<Account>,
    context_id: Option<String>,
    installed_source: Option<String>,
    catalogue_approved: Option<Value>,
    source_approval: Option<String>,
    candidate: Option<String>,
    register_approved: bool,
    trial_number: u32,
    trial_approval: Option<String>,
    decision: Option<Decision>,
    review_digest: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Reference {
    example: String,
    criterion: Option<String>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Account {
    origin: String,
    user_id: Uuid,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
struct Decision {
    evidence: String,
    passed: bool,
}

impl State {
    fn new(directory: PathBuf) -> Self {
        Self {
            version: VERSION,
            directory,
            project: None,
            local_check: None,
            reference: None,
            account: None,
            context_id: None,
            installed_source: None,
            catalogue_approved: None,
            source_approval: None,
            candidate: None,
            register_approved: false,
            trial_number: 1,
            trial_approval: None,
            decision: None,
            review_digest: None,
        }
    }

    fn save(&self) -> Result<(), Error> {
        config::write_private(&self.directory.join(STATE), self, true)
    }

    fn release(&self) -> PathBuf {
        self.directory.join("release")
    }

    fn trial(&self) -> PathBuf {
        self.directory.join(format!("trial-{}", self.trial_number))
    }

    fn next_trial(&mut self) -> Result<(), Error> {
        self.trial_number = self
            .trial_number
            .checked_add(1)
            .ok_or_else(|| invalid("Trial limit reached."))?;
        self.trial_approval = None;
        self.decision = None;
        self.review_digest = None;
        self.save()
    }
}

struct Prompt<R, W> {
    input: R,
    output: W,
}

impl<R: BufRead, W: Write> Prompt<R, W> {
    fn note(&mut self, message: &str) -> Result<(), Error> {
        writeln!(self.output, "{}", safe_text(message, true))?;
        Ok(())
    }

    fn show(&mut self, label: &str, value: &Value) -> Result<(), Error> {
        self.note(&format!(
            "{label}\n{}",
            serde_json::to_string_pretty(value)?
        ))
    }

    fn ask(&mut self, label: &str, default: Option<&str>) -> Result<String, Error> {
        loop {
            let suffix = default.map_or(String::new(), |value| format!(" (Enter for {value})"));
            writeln!(
                self.output,
                "{}{}",
                safe_text(label, false),
                safe_text(&suffix, false)
            )?;
            write!(self.output, "> ")?;
            self.output.flush()?;
            let mut value = String::new();
            if self.input.read_line(&mut value)? == 0 {
                return Err(invalid(
                    "Input ended. Resume with the same model setup directory.",
                ));
            }
            let value = value.trim();
            if !value.is_empty() {
                return Ok(value.to_owned());
            }
            if let Some(value) = default {
                return Ok(value.to_owned());
            }
        }
    }

    fn choice(&mut self, label: &str, default: &str, choices: &[&str]) -> Result<String, Error> {
        loop {
            let answer = self.ask(label, Some(default))?;
            if choices.contains(&answer.as_str()) {
                return Ok(answer);
            }
            self.note(&format!("Choose {}.", choices.join(" or ")))?;
        }
    }

    fn yes(&mut self, label: &str) -> Result<bool, Error> {
        Ok(matches!(
            self.choice(label, "no", &["yes", "y", "no", "n"])?.as_str(),
            "yes" | "y"
        ))
    }
}

pub(crate) fn execute(args: &Args, options: &ModelSetup) -> Result<Completion, Failure> {
    let inspecting = options.command.is_some();
    let root = match &options.command {
        Some(ModelSetupCommand::Status { directory }) => directory,
        None => options
            .directory
            .as_ref()
            .ok_or_else(|| invalid("Supply --directory."))?,
    };
    if !inspecting && (!io::stdin().is_terminal() || args.json) {
        return Err(invalid(
            "Run model setup in a terminal. Use model setup status for noninteractive inspection.",
        )
        .into());
    }
    let created = !inspecting && !exists(root)?;
    if created {
        fs::DirBuilder::new().mode(0o700).create(root)?;
    }
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid("Model setup requires a private real directory (0700).").into());
    }
    let root = root.canonicalize()?;
    let _lock = if inspecting {
        None
    } else {
        Some(config::lock_private(
            &root.join("model-setup"),
            "Model setup is already running in this directory.",
        )?)
    };
    let checkpoint = root.join(STATE);
    let mut state = if !inspecting && !exists(&checkpoint)? {
        for entry in fs::read_dir(&root)? {
            if entry?.file_name() != "model-setup.lock" {
                return Err(invalid("This directory has no model setup checkpoint. Preserve its files and choose a new directory.").into());
            }
        }
        let state = State::new(root);
        config::write_private(&checkpoint, &state, false)?;
        state
    } else {
        config::require_private(&checkpoint)?;
        let state: State = config::read_json(&checkpoint)?;
        if state.version != VERSION || state.directory != root || state.trial_number == 0 {
            return Err(invalid(
                "This model setup checkpoint is incompatible or is outside its original directory.",
            )
            .into());
        }
        state
    };
    if inspecting {
        let view = status(args, &state)?;
        return Ok(Completion {
            value: Some(view),
            exit: 0,
        });
    }
    let mut prompt = Prompt {
        input: io::stdin().lock(),
        output: io::stderr(),
    };
    let view = advance(args, &mut state, &mut prompt)?;
    let exit = if matches!(
        view["stage"].as_str(),
        Some("ready_for_activation" | "activated")
    ) {
        0
    } else {
        1
    };
    Ok(Completion {
        value: Some(view),
        exit,
    })
}

fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn digest(value: &Value) -> Result<String, Error> {
    Ok(Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn local_key(source: &ParsedPublication) -> Result<String, Error> {
    digest(&json!({
        "model_id":source.model.id, "definition":source.definition,
        "entrypoint":source.execution.entrypoint, "slurm_script":source.execution.slurm_script,
        "environment":source.execution.environment,
    }))
}

fn named<'a>(
    source: &'a ParsedPublication,
    reference: &Reference,
) -> Result<&'a ScientificExample, Error> {
    source
        .definition
        .examples
        .iter()
        .find(|example| example.name == reference.example)
        .ok_or_else(|| invalid("The selected example is no longer in the frozen publication."))
}

fn choose_project<R: BufRead, W: Write>(
    state: &mut State,
    prompt: &mut Prompt<R, W>,
) -> Result<(), Error> {
    let choice = prompt.choice(
        "Start from a new starter or an existing publication? [starter/existing]",
        "starter",
        &["starter", "existing"],
    )?;
    let path = if choice == "starter" {
        let id = prompt.ask("New model ID", None)?;
        let kind = if prompt.choice(
            "Implementation [python/executable]",
            "python",
            &["python", "executable"],
        )? == "python"
        {
            StarterKind::Python
        } else {
            StarterKind::Executable
        };
        let io = if prompt.choice(
            "Inputs and outputs [json/files]",
            "json",
            &["json", "files"],
        )? == "json"
        {
            StarterIo::Json
        } else {
            StarterIo::Files
        };
        let driver = if prompt.choice(
            "Execution driver [process/slurm]",
            "process",
            &["process", "slurm"],
        )? == "process"
        {
            StarterDriver::Process
        } else {
            StarterDriver::Slurm
        };
        let project = state.directory.join("project");
        starter::init(&id, &project, kind, io, driver)?;
        prompt.note(&format!(
            "Created {}. From that directory run python3 -B -m unittest discover -s . -p 'test_*.py'. Adapt the wrapper, declaration and fixtures for your model before confirming local tests.",
            project.display()
        ))?;
        project.join("publication.json")
    } else {
        PathBuf::from(prompt.ask("Publication file", Some("publication.json"))?).canonicalize()?
    };
    manifest::publication_source(&path)?;
    state.project = Some(path);
    state.save()
}

fn choose_reference<R: BufRead, W: Write>(
    source: &ParsedPublication,
    prompt: &mut Prompt<R, W>,
) -> Result<Option<Reference>, Error> {
    let Some(first) = source.definition.examples.first() else {
        return Ok(None);
    };
    let examples: Vec<_> = source
        .definition
        .examples
        .iter()
        .map(|example| example.name.as_str())
        .collect();
    prompt.show("Named examples", &json!(examples))?;
    let example = prompt.choice("Named example", &first.name, &examples)?;
    let selected = source
        .definition
        .examples
        .iter()
        .find(|entry| entry.name == example)
        .expect("validated choice");
    let exact = selected.outputs.as_ref().is_some_and(Value::is_object)
        && files::declarations(&source.definition)
            .map_err(|error| invalid(error.to_string()))?
            .is_empty()
        && files::output_declarations(&source.definition)
            .map_err(|error| invalid(error.to_string()))?
            .is_empty();
    let method = if exact {
        prompt.choice(
            "Check the reference with exact JSON or author review? [exact/author]",
            "exact",
            &["exact", "author"],
        )?
    } else {
        "author".into()
    };
    let criterion = if method == "author" {
        Some(prompt.ask("Expected result and acceptance criterion (include tolerance or reference files as needed)", None)?)
    } else {
        None
    };
    Ok(Some(Reference { example, criterion }))
}

fn view(
    args: &Args,
    state: &State,
    model: Option<&str>,
    stage: &str,
    owner: &str,
    next: &str,
) -> Value {
    let activation = (stage == "ready_for_activation").then(|| {
        invocation(
            args,
            &[
                "model".into(),
                "release".into(),
                "activate".into(),
                state.release().to_string_lossy().into_owned(),
            ],
        )
    });
    let resume = invocation(
        args,
        &[
            "model".into(),
            "setup".into(),
            "--directory".into(),
            state.directory.to_string_lossy().into_owned(),
        ],
    );
    let stage_label = if stage == "ready_for_activation" {
        "Ready for activation".into()
    } else {
        stage.replace('_', " ")
    };
    json!({
        "schema_version": VERSION, "directory": state.directory, "stage": stage, "stage_label": stage_label,
        "model_id": model, "context_id": state.context_id, "publication_id": state.candidate, "run_id": null,
        "next_action": next, "owner": owner, "resume_command": resume, "activation_command": activation,
    })
}

fn client(args: &Args) -> Result<EnumerayClient, Error> {
    let path = args
        .config
        .clone()
        .map(Ok)
        .unwrap_or_else(config::default_path)?;
    config::client(args, &config::load(&path)?)
}

fn authenticate(client: &EnumerayClient, state: &State) -> Result<Option<Account>, Error> {
    if state
        .account
        .as_ref()
        .is_some_and(|account| account.origin != client.base_url())
    {
        return Err(invalid("Resume model setup with its original API origin."));
    }
    match client.me() {
        Ok(me) => {
            let account = Account {
                origin: client.base_url().into(),
                user_id: me.user_id,
            };
            if state
                .account
                .as_ref()
                .is_some_and(|saved| *saved != account)
            {
                return Err(invalid("Resume model setup with its original account."));
            }
            Ok(Some(account))
        }
        Err(Error::Api { status: 401, .. }) | Err(Error::Session { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn grants(client: &EnumerayClient) -> Result<Vec<AvailableGrant>, Error> {
    let mut cursor = None;
    let mut result = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let query = cursor.as_deref().map(|cursor| [("cursor", cursor)]);
        let page = client
            .management()
            .available_compute_grants(query.as_ref().map_or(&[], |value| value.as_slice()))?;
        result.extend(page.items);
        let Some(next) = page.next_cursor else {
            return Ok(result);
        };
        if !seen.insert(next.clone()) {
            return Err(invalid("Compute grant pagination repeated a cursor."));
        }
        cursor = Some(next);
    }
}

fn selected_installation(source: &ParsedPublication, grant: &AvailableGrant) -> Option<String> {
    let path = source.execution.release_path.as_ref()?;
    let destination = Path::new(path);
    if source.execution.context_id.as_deref() != Some(&grant.context_id)
        || !grant.context.installation_roots.iter().any(|root| {
            let root = Path::new(root);
            destination != root && destination.starts_with(root)
        })
        || (grant.context.driver == "slurm") != source.execution.slurm_script.is_some()
    {
        return None;
    }
    Some(path.clone())
}

fn installation_action(source: &ParsedPublication, grant: &AvailableGrant) -> String {
    let proposed = selected_installation(source, grant).unwrap_or_else(|| {
        let root = grant
            .context
            .installation_roots
            .first()
            .map_or("<approved-root>", String::as_str);
        format!("{}/{}/r1", root.trim_end_matches('/'), source.model.id)
    });
    format!(
        "Use context {} and a new versioned path such as {}. Set execution.context_id and execution.release_path in publication.json. Ask the site administrator to install the wrapper, declaration, runtime and datasets there, verify access as the execution account, and prepare site resources and the batch script for Slurm.",
        grant.context_id, proposed
    )
}

fn advance<R: BufRead, W: Write>(
    args: &Args,
    state: &mut State,
    prompt: &mut Prompt<R, W>,
) -> Result<Value, Failure> {
    if state.project.is_none() {
        choose_project(state, prompt)?;
    }
    let path = state.project.as_ref().expect("selected project").clone();
    let (raw, source) = manifest::publication_source(&path)?;
    let model = source.model.id.as_str();
    let pause = |state, stage, owner, next| view(args, state, Some(model), stage, owner, next);
    let source_digest = digest(&raw)?;
    if state
        .source_approval
        .as_ref()
        .is_some_and(|saved| saved != &source_digest)
    {
        return Ok(pause(
            state,
            "source_changed",
            "model author",
            "Use a new model setup directory for the revised declaration. The original candidate and evidence remain in this directory.",
        ));
    }
    if !collections::declarations(&source.definition, false)
        .map_err(|e| invalid(e.to_string()))?
        .is_empty()
        || !collections::declarations(&source.definition, true)
            .map_err(|e| invalid(e.to_string()))?
            .is_empty()
    {
        return Ok(pause(
            state,
            "collection_workflow",
            "model author",
            "Use model release and analysis commands directly for publications with collections.",
        ));
    }
    let key = local_key(&source)?;
    if state.local_check.as_ref() != Some(&key) {
        prompt.note(&format!("Validated the editable declaration at {}. Run your local wrapper tests with a known result before continuing.", path.display()))?;
        if !prompt.yes("Have you tested this model locally?")? {
            return Ok(pause(
                state,
                "local_checks",
                "model author",
                "Run your local wrapper and reference tests, then resume.",
            ));
        }
        state.local_check = Some(key);
        state.reference = None;
        state.save()?;
    }
    if state.reference.is_none() {
        state.reference = choose_reference(&source, prompt)?;
        if state.reference.is_none() {
            return Ok(pause(
                state,
                "reference_case",
                "model author",
                "Add a named example to the editable publication and resume.",
            ));
        }
        state.save()?;
    }

    let client = client(args)?;
    let Some(account) = authenticate(&client, state)? else {
        let command = invocation(args, &["auth".into(), "login".into()]);
        return Ok(pause(
            state,
            "sign_in",
            "model author",
            &format!("Sign in with {command} and resume."),
        ));
    };
    if state.account.is_none() {
        state.account = Some(account);
        state.save()?;
    }

    if state.source_approval.is_none() {
        let grants = grants(&client)?;
        if grants.is_empty() {
            let user = state.account.as_ref().expect("signed in").user_id;
            return Ok(pause(
                state,
                "compute_grant",
                "connector owner",
                &format!(
                    "Request a compute grant for author account {user} and confirm where the model can be installed."
                ),
            ));
        }
        let valid_context = |id: &str| grants.iter().any(|grant| grant.context_id == id);
        let declaration_context_changed =
            source.execution.context_id.as_deref().is_some_and(|id| {
                valid_context(id) && state.context_id.as_deref().is_some_and(|saved| saved != id)
            });
        if !state.context_id.as_deref().is_some_and(valid_context) || declaration_context_changed {
            let choices: Vec<_> = grants
                .iter()
                .map(|grant| grant.context_id.as_str())
                .collect();
            let contexts: Vec<_> = grants.iter().map(|grant| json!({
                "id":grant.context_id, "connector":grant.connector_label, "driver":grant.context.driver,
                "connection":grant.context.connection, "destination":grant.context.metadata.location,
                "installation_roots":grant.context.installation_roots,
            })).collect();
            prompt.show("Granted compute contexts", &json!(contexts))?;
            let default = source
                .execution
                .context_id
                .as_deref()
                .filter(|id| valid_context(id))
                .unwrap_or(choices[0]);
            state.context_id = Some(prompt.choice("Context ID", default, &choices)?);
            state.save()?;
        }
        let grant = grants
            .iter()
            .find(|grant| Some(&grant.context_id) == state.context_id.as_ref())
            .expect("selected active grant");
        let handoff = installation_action(&source, grant);
        let Some(destination) = selected_installation(&source, grant) else {
            return Ok(pause(
                state,
                "installation",
                "model author and site administrator",
                &handoff,
            ));
        };
        source
            .candidate("validation-only")
            .map_err(|error| invalid(error.to_string()))?;
        if state.installed_source.as_ref() != Some(&source_digest) {
            prompt.note(&handoff)?;
            if !prompt.yes("Has this version been installed at the approved path?")? {
                return Ok(pause(state, "installation", "site administrator", &handoff));
            }
            state.installed_source = Some(source_digest.clone());
            state.save()?;
        }
        if !catalogue(&client, state, &source, prompt)? {
            return Ok(pause(
                state,
                "catalogue_approval",
                "model author",
                "Review and confirm catalogue creation, or choose a model you administer.",
            ));
        }
        prompt.note(&format!(
            "Preparing the release of {model} in context {} at {destination}.",
            grant.context_id
        ))?;
        state.source_approval = Some(source_digest);
        state.save()?;
    }
    if state.candidate.is_none() {
        let initialized = if exists(&state.release())? {
            // Initialization may have finished just before the wizard checkpoint was saved.
            release::wizard_status(args, &client, &state.release(), model, &raw)?
        } else {
            release::wizard_init(args, &client, &state.release(), model, &raw)?
        };
        state.candidate = Some(text_field(&initialized, "publication_id")?.into());
        state.save()?;
    }
    let candidate = state.candidate.as_ref().expect("saved candidate").clone();
    let mut release_view = release::wizard_status(args, &client, &state.release(), model, &raw)?;
    if release_view["publication_id"] != candidate {
        return Err(invalid("Release candidate differs from the model setup checkpoint.").into());
    }
    if release_view["stage"] == "activated" {
        return Ok(pause(
            state,
            "activated",
            "model author",
            "Inspect the current publication with model get.",
        ));
    }
    if !state.register_approved {
        prompt.show(
            "Proposed candidate",
            &release_view["comparison"]["candidate"],
        )?;
        if !prompt.yes("Register this candidate?")? {
            return Ok(pause(
                state,
                "registration_approval",
                "model author",
                "Review the candidate and confirm registration.",
            ));
        }
        state.register_approved = true;
        state.save()?;
    }
    if matches!(
        release_view["stage"].as_str(),
        Some("validating" | "prepared")
    ) {
        match release::execute(
            args,
            &client,
            &Release::Resume {
                directory: state.release(),
                interval: 2,
                deadline: 20,
            },
        ) {
            Err(failure) if failure.exit == 5 => {
                return Ok(pause(
                    state,
                    "candidate_pending",
                    "connector owner",
                    "Waiting for the connector to validate this installation. Resume to check the same candidate.",
                ));
            }
            result => {
                result?;
            }
        }
        release_view = release::wizard_status(args, &client, &state.release(), model, &raw)?;
    }
    if let Some(pending) = release_pending(args, state, model, &release_view) {
        return Ok(pending);
    }
    let reference = state.reference.as_ref().expect("saved reference");
    let example = named(&source, reference)?;
    trial(
        args, state, &client, &source, &raw, &candidate, example, prompt,
    )
}

fn text_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("Workflow evidence omitted {field}.")))
}

fn catalogue<R: BufRead, W: Write>(
    client: &EnumerayClient,
    state: &mut State,
    source: &ParsedPublication,
    prompt: &mut Prompt<R, W>,
) -> Result<bool, Error> {
    let request = ModelCreate {
        model: source.model.clone(),
    };
    let value = serde_json::to_value(&request)?;
    if state
        .catalogue_approved
        .as_ref()
        .is_some_and(|approved| !equivalent(approved, &value))
    {
        return Err(invalid(
            "Catalogue metadata changed after creation approval. Restore the reviewed metadata or use a new setup directory.",
        ));
    }
    let response = if state.catalogue_approved.is_some() {
        // Repeat the immutable, idempotent request if its previous reply was interrupted.
        client.management().create_model(&request)?
    } else {
        match client.management().managed_model(&source.model.id) {
            Ok(model) => model,
            Err(Error::Api { status: 404, .. }) => {
                prompt.show("New catalogue model", &value)?;
                if !prompt.yes("Create this catalogue model?")? {
                    return Ok(false);
                }
                state.catalogue_approved = Some(value);
                state.save()?;
                client.management().create_model(&request)?
            }
            Err(error) => return Err(error),
        }
    };
    if response.model_id != source.model.id || !response.can_manage {
        return Err(invalid(
            "Model administration is required. Use an owner or co-admin account, or choose another model ID.",
        ));
    }
    Ok(true)
}

fn release_pending(args: &Args, state: &State, model: &str, release: &Value) -> Option<Value> {
    let make = |stage, owner, message| view(args, state, Some(model), stage, owner, message);
    match release["stage"].as_str() {
        Some("activated") => Some(make(
            "activated",
            "model author",
            "Inspect the current publication with model get.",
        )),
        Some("blocked") => {
            let owner = match release["blockers"][0].as_str() {
                Some("model_administration_required") => "model owner",
                Some("compute_authorization_unavailable" | "captured_grant_unavailable") => {
                    "connector owner"
                }
                _ => "model author and site administrator",
            };
            let mut value = make(
                "candidate_blocked",
                owner,
                release["validation_message"]
                    .as_str()
                    .or(release["next_action"].as_str())
                    .unwrap_or("Inspect the saved release status."),
            );
            value["blockers"] = release["blockers"].clone();
            Some(value)
        }
        Some("ready" | "reviewed") => None,
        _ if !state.register_approved => Some(make(
            "registration_approval",
            "model author",
            "Review the candidate and confirm registration.",
        )),
        _ => Some(make(
            "candidate_pending",
            "connector owner",
            "Waiting for the connector to validate this installation. Resume to check the same candidate.",
        )),
    }
}

fn create_trial<R: BufRead, W: Write>(
    args: &Args,
    client: &EnumerayClient,
    state: &State,
    source: &ParsedPublication,
    candidate: &str,
    example: &ScientificExample,
    prompt: &mut Prompt<R, W>,
) -> Result<(), Failure> {
    let references = files::references(&source.definition, &example.inputs)
        .map_err(|error| invalid(error.to_string()))?;
    let mut file = Vec::new();
    let mut media_type = Vec::new();
    for reference in references {
        let name = reference.input_name;
        let path = prompt.ask(&format!("Local file for {name}"), None)?;
        file.push(format!("{name}={path}"));
        let choices: Vec<_> = reference
            .declaration
            .media_types
            .iter()
            .map(String::as_str)
            .collect();
        if choices.len() > 1 {
            let selected =
                prompt.choice(&format!("Media type for {name}"), choices[0], &choices)?;
            media_type.push(format!("{name}={selected}"));
        }
    }
    analysis::execute(
        args,
        client,
        &Analysis::Init {
            model: source.model.id.clone(),
            candidate: Some(candidate.into()),
            directory: state.trial(),
            example: Some(example.name.clone()),
            inputs: None,
            file,
            media_type,
            collection: vec![],
            collection_manifest: vec![],
            metadata: vec![],
        },
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn trial<R: BufRead, W: Write>(
    args: &Args,
    state: &mut State,
    client: &EnumerayClient,
    source: &ParsedPublication,
    raw: &Value,
    candidate: &str,
    example: &ScientificExample,
    prompt: &mut Prompt<R, W>,
) -> Result<Value, Failure> {
    let model = source.model.id.as_str();
    if !exists(&state.trial())? {
        if state.trial_approval.is_some() || state.decision.is_some() {
            return Err(invalid(
                "Restore the original trial directory to recover its accepted run.",
            )
            .into());
        }
        create_trial(args, client, state, source, candidate, example, prompt)?;
    }
    let mut observed = analysis::wizard_status(client, &state.trial(), model, candidate)?;
    if observed["run_id"].is_null() {
        let preview =
            match analysis::wizard_preview(client, &state.trial(), model, candidate, example) {
                Ok(preview) => preview,
                Err(error) => {
                    return recover_trial_inputs(args, state, model, &observed, error, prompt);
                }
            };
        let preview_digest = digest(&preview)?;
        if state.trial_approval.as_ref() != Some(&preview_digest) {
            prompt.show("Trial to submit", &preview)?;
            if let Some(criterion) = state
                .reference
                .as_ref()
                .and_then(|reference| reference.criterion.as_deref())
            {
                prompt.note(&format!("Acceptance criterion: {criterion}"))?;
            }
            if !prompt.yes("Submit this trial?")? {
                return Ok(trial_view(
                    args,
                    state,
                    model,
                    &observed,
                    "trial_approval",
                    "Review inputs and destination, then resume to confirm submission.",
                ));
            }
            state.trial_approval = Some(preview_digest);
            state.save()?;
        }
        if let Err(error) =
            analysis::wizard_submit(client, &state.trial(), model, candidate, example, &preview)
        {
            let current = analysis::wizard_status(client, &state.trial(), model, candidate)?;
            return recover_trial_inputs(args, state, model, &current, error, prompt);
        }
        observed = analysis::wizard_status(client, &state.trial(), model, candidate)?;
    }
    if !matches!(
        observed["run"]["state"].as_str(),
        Some("succeeded" | "failed" | "cancelled")
    ) {
        return Ok(trial_view(
            args,
            state,
            model,
            &observed,
            "trial_running",
            "Resume to inspect this same run and collect its terminal result.",
        ));
    }
    if observed["run"]["state"] != "succeeded" {
        let view = trial_view(
            args,
            state,
            model,
            &observed,
            "trial_failed",
            "Inspect the saved run logs. Resume this wizard to explicitly start a new trial.",
        );
        if prompt.yes("Start another trial of this candidate?")? {
            state.next_trial()?;
            return Ok(trial_view(
                args,
                state,
                model,
                &Value::Null,
                "trial_approval",
                "Resume to prepare and approve the new trial. The previous run is retained.",
            ));
        }
        return Ok(view);
    }
    require_trial_approval(state, &observed, example)?;
    if observed["collection"]["complete"] != true {
        observed = collect_trial(args, client, state, model, candidate)?;
    }
    if observed["collection"]["complete"] != true {
        return Ok(trial_view(
            args,
            state,
            model,
            &observed,
            "collection_pending",
            "Resume to finish verified collection for this run.",
        ));
    }
    let expected = example.outputs.as_ref().unwrap_or(&Value::Null);
    let outcome = &observed["run"]["result"]["outputs"];
    let evidence = evidence_key(&observed, candidate)?;
    if state
        .decision
        .as_ref()
        .is_none_or(|decision| decision.evidence != evidence)
    {
        let criterion = state
            .reference
            .as_ref()
            .and_then(|reference| reference.criterion.as_deref());
        prompt.show(
            "Collected reference evidence",
            &json!({
                "run_id": observed["run_id"], "expected":expected, "actual":outcome,
                "criterion":criterion, "bundle":observed["bundle"],
            }),
        )?;
        let passed = if criterion.is_some() {
            let accepted = prompt.yes("Does the collected result meet the recorded criterion?")?;
            if accepted {
                // Reverify the bytes on disk before retaining acceptance of file results.
                let collected = collect_trial(args, client, state, model, candidate)?;
                if collected["collection"]["complete"] != true
                    || evidence_key(&collected, candidate)? != evidence
                {
                    return Err(invalid("Collected evidence changed. Inspect the verified result before accepting it.").into());
                }
            }
            accepted
        } else {
            exact_result(example, &observed)
        };
        state.decision = Some(Decision { evidence, passed });
        state.review_digest = None;
        state.save()?;
    }
    if !state
        .decision
        .as_ref()
        .is_some_and(|decision| decision.passed)
    {
        let view = trial_view(
            args,
            state,
            model,
            &observed,
            "result_mismatch",
            "The collected result failed the reference check. Inspect the evidence before trying again.",
        );
        if prompt.yes("Start another trial of this candidate?")? {
            state.next_trial()?;
            return Ok(trial_view(
                args,
                state,
                model,
                &Value::Null,
                "trial_approval",
                "Resume to prepare and approve the new trial. The previous result is retained.",
            ));
        }
        return Ok(view);
    }
    let mut release_view = release::wizard_status(args, client, &state.release(), model, raw)?;
    if let Some(pending) = release_pending(args, state, model, &release_view) {
        return Ok(pending);
    }
    if !review_current(state, &release_view)? {
        let comparison = &release_view["review_snapshot"];
        prompt.show("Current and proposed publication", comparison)?;
        prompt.show("Candidate trial evidence", &release_view["trial_evidence"])?;
        if !prompt.yes("Save this release review?")? {
            return Ok(trial_view(
                args,
                state,
                model,
                &observed,
                "release_review",
                "Review the differences and confirm to save the activation guard.",
            ));
        }
        let reviewed = digest(comparison)?;
        release_view =
            release::wizard_review(args, client, &state.release(), model, raw, comparison)?;
        state.review_digest = Some(reviewed);
        state.save()?;
    }
    Ok(ready_view(args, state, model, &observed, &release_view)?)
}

fn recover_trial_inputs<R: BufRead, W: Write>(
    args: &Args,
    state: &mut State,
    model: &str,
    observed: &Value,
    error: Error,
    prompt: &mut Prompt<R, W>,
) -> Result<Value, Failure> {
    if observed["submission_started"] != false || !matches!(error, Error::Invalid(_)) {
        return Err(error.into());
    }
    prompt.note(&error.to_string())?;
    if prompt.yes("Prepare a new trial with fresh inputs?")? {
        state.next_trial()?;
        return Ok(trial_view(
            args,
            state,
            model,
            &Value::Null,
            "trial_approval",
            "Resume to prepare and approve the new trial. The previous preparation is retained.",
        ));
    }
    Err(error.into())
}

fn require_trial_approval(
    state: &State,
    observed: &Value,
    example: &ScientificExample,
) -> Result<(), Error> {
    let mut frozen = observed["frozen_review"].clone();
    if let Some(object) = frozen.as_object_mut() {
        object.insert("example".into(), json!(example.name));
        if state.trial_approval.as_ref() == Some(&digest(&frozen)?) {
            return Ok(());
        }
    }
    Err(invalid(
        "This run differs from the wizard's trial approval. Use the existing release and analysis commands to inspect an externally submitted trial.",
    ))
}

fn collect_trial(
    args: &Args,
    client: &EnumerayClient,
    state: &State,
    model: &str,
    candidate: &str,
) -> Result<Value, Failure> {
    let directory = state.trial();
    analysis::execute(
        args,
        client,
        &Analysis::Collect {
            directory: directory.clone(),
            member: vec![],
            evidence_only: false,
        },
    )?;
    Ok(analysis::wizard_status(
        client, &directory, model, candidate,
    )?)
}

fn evidence_key(observed: &Value, candidate: &str) -> Result<String, Error> {
    let run = text_field(observed, "run_id")?;
    let bundle = text_field(observed, "bundle_sha256")?;
    if observed["run"]["publication_id"] != candidate
        || observed["run"]["run_id"] != run
        || !observed["run"]["result"]["outputs"].is_object()
    {
        return Err(invalid(
            "Collected result does not identify the saved candidate trial.",
        ));
    }
    digest(&json!([
        candidate,
        run,
        bundle,
        observed["run"]["effective_inputs"],
        observed["run"]["result"]["outputs"]
    ]))
}

fn exact_result(example: &ScientificExample, observed: &Value) -> bool {
    equivalent(&observed["run"]["effective_inputs"], &example.inputs)
        && example
            .outputs
            .as_ref()
            .is_some_and(|expected| equivalent(&observed["run"]["result"]["outputs"], expected))
}

fn trial_view(
    args: &Args,
    state: &State,
    model: &str,
    trial: &Value,
    stage: &str,
    next: &str,
) -> Value {
    let mut view = view(args, state, Some(model), stage, "model author", next);
    view["run_id"] = trial["run_id"].clone();
    view["trial_directory"] = json!(state.trial());
    view["collection_complete"] = trial["collection"]["complete"].clone();
    view
}

fn review_current(state: &State, release: &Value) -> Result<bool, Error> {
    Ok(release["stage"] == "reviewed"
        && release["guard_changed"] == false
        && release["review_snapshot"].is_object()
        && state.review_digest.as_ref() == Some(&digest(&release["review_snapshot"])?))
}

fn ready_view(
    args: &Args,
    state: &State,
    model: &str,
    trial: &Value,
    release: &Value,
) -> Result<Value, Error> {
    Ok(if review_current(state, release)? {
        trial_view(
            args,
            state,
            model,
            trial,
            "ready_for_activation",
            "Make this reviewed candidate current with the explicit activation command.",
        )
    } else {
        trial_view(
            args,
            state,
            model,
            trial,
            "release_review",
            "Resume to review the current comparison and save the activation guard.",
        )
    })
}

fn status(args: &Args, state: &State) -> Result<Value, Failure> {
    let Some(project) = &state.project else {
        return Ok(view(
            args,
            state,
            None,
            "project",
            "model author",
            "Resume to create a starter or open a declaration.",
        ));
    };
    let (raw, source) = manifest::publication_source(project)?;
    let model = source.model.id.as_str();
    let pause = |stage, owner, next| view(args, state, Some(model), stage, owner, next);
    let source_digest = digest(&raw)?;
    if state
        .source_approval
        .as_ref()
        .is_some_and(|saved| saved != &source_digest)
    {
        return Ok(pause(
            "source_changed",
            "model author",
            "Use a new setup directory for the revised declaration.",
        ));
    }
    if state.local_check.as_ref() != Some(&local_key(&source)?) {
        return Ok(pause(
            "local_checks",
            "model author",
            "Run your local wrapper and reference tests, then resume.",
        ));
    }
    if state.reference.is_none() {
        return Ok(pause(
            "reference_case",
            "model author",
            "Resume to select a named example and reference result.",
        ));
    }
    if state.account.is_none() {
        return Ok(pause(
            "sign_in",
            "model author",
            "Sign in and resume to choose from granted contexts.",
        ));
    }
    let client = client(args)?;
    if authenticate(&client, state)?.is_none() {
        return Ok(pause(
            "sign_in",
            "model author",
            "Sign in again with the original account and resume.",
        ));
    }
    if state.source_approval.is_none() {
        let grants = grants(&client)?;
        let Some(grant) = grants
            .iter()
            .find(|grant| Some(&grant.context_id) == state.context_id.as_ref())
        else {
            return if grants.is_empty() {
                Ok(pause(
                    "compute_grant",
                    "connector owner",
                    "Request a compute grant for this author account.",
                ))
            } else {
                Ok(pause(
                    "context_selection",
                    "model author",
                    "Resume to choose a granted execution context.",
                ))
            };
        };
        if state.installed_source.as_ref() == Some(&source_digest) {
            return Ok(pause(
                "catalogue_approval",
                "model author",
                "Resume to confirm catalogue creation or use the model you administer.",
            ));
        }
        return Ok(pause(
            "installation",
            "model author and site administrator",
            &installation_action(&source, grant),
        ));
    }
    if !exists(&state.release())? {
        return Ok(pause(
            "registration_approval",
            "model author",
            "Resume to prepare and register the reviewed candidate.",
        ));
    }
    let release = release::wizard_status(args, &client, &state.release(), model, &raw)?;
    let candidate = text_field(&release, "publication_id")?;
    if let Some(saved) = &state.candidate
        && saved != candidate
    {
        return Err(invalid("Release candidate differs from the model setup checkpoint.").into());
    }
    if let Some(mut pending) = release_pending(args, state, model, &release) {
        pending["publication_id"] = json!(candidate);
        return Ok(pending);
    }
    if !exists(&state.trial())? {
        return Ok(pause(
            "trial_approval",
            "model author",
            "Resume to prepare a trial and approve its submission.",
        ));
    }
    let trial = analysis::wizard_status(&client, &state.trial(), model, candidate)?;
    let (stage, next) = match trial["run"]["state"].as_str() {
        Some("failed" | "cancelled") => (
            "trial_failed",
            "Inspect the run logs and decide whether to retry.",
        ),
        Some("succeeded") if trial["collection"]["complete"] != true => {
            ("collection_pending", "Resume to collect verified results.")
        }
        Some("succeeded") => {
            require_trial_approval(
                state,
                &trial,
                named(&source, state.reference.as_ref().expect("saved reference"))?,
            )?;
            let evidence = evidence_key(&trial, candidate)?;
            match state
                .decision
                .as_ref()
                .filter(|decision| decision.evidence == evidence)
            {
                Some(decision) if decision.passed => {
                    return Ok(ready_view(args, state, model, &trial, &release)?);
                }
                Some(_) => (
                    "result_mismatch",
                    "Inspect the collected results and the reference.",
                ),
                None => (
                    "result_review",
                    "Resume to check the collected result against the reference.",
                ),
            }
        }
        _ if trial["run_id"].is_null() => (
            "trial_approval",
            "Resume to approve or reconcile submission of this trial.",
        ),
        _ => ("trial_running", "Resume to inspect this same run."),
    };
    Ok(trial_view(args, state, model, &trial, stage, next))
}

#[cfg(test)]
mod tests;

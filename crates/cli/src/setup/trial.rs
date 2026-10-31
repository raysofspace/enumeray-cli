//! Reuse release and analysis operations rather than introducing execution retries.
use super::*;
use crate::args::{Analysis, Model, Release, StarterDriver, StarterIo, StarterKind};
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Progress {
    #[serde(default)]
    source_prepared: bool,
    #[serde(default)]
    publication_sha256: Option<String>,
    #[serde(default)]
    candidate_id: Option<String>,
    #[serde(default)]
    run_key: Option<String>,
    #[serde(default)]
    destination: Option<PathBuf>,
    #[serde(default)]
    model_started: bool,
    #[serde(default)]
    model_registered: bool,
    #[serde(default)]
    complete: bool,
}
#[derive(Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    sha256: String,
    executable: bool,
}
fn list(root: &Path, relative: &Path, entries: &mut Vec<Entry>) -> Result<(), Error> {
    let mut files = fs::read_dir(root.join(relative))?.collect::<Result<Vec<_>, _>>()?;
    files.sort_by_key(|e| e.file_name());
    for file in files {
        if matches!(
            file.file_name().to_str(),
            Some(".git" | "target" | "__pycache__")
        ) {
            continue;
        }
        let path = relative.join(file.file_name());
        let m = fs::symlink_metadata(file.path())?;
        if m.file_type().is_symlink() {
            return Err(invalid(
                "Automatic project staging refuses symbolic links; provide an already installed release through the site's installation route.",
            ));
        }
        if m.is_dir() {
            list(root, &path, entries)?;
        } else if m.is_file() {
            if m.len() > 256 * 1024 * 1024 || entries.len() >= 10000 {
                return Err(invalid(
                    "Project exceeds bounded automatic staging; use an existing site installation.",
                ));
            }
            entries.push(Entry {
                path,
                sha256: digest_bytes(fs::read(file.path())?),
                executable: m.permissions().mode() & 0o111 != 0,
            });
        } else {
            return Err(invalid(
                "Project staging requires regular files and directories.",
            ));
        }
    }
    Ok(())
}
fn install(source: &Path, destination: &Path, manifest: &Path) -> Result<(), Error> {
    let mut files = vec![];
    list(source, Path::new(""), &mut files)?;
    if manifest.exists() {
        let old: Vec<Entry> = read(manifest)?;
        if hash(&old)? != hash(&files)? {
            return Err(invalid(
                "Project changed after installation staging began. Preserve this operation; a changed release needs a new setup/project operation.",
            ));
        }
    } else {
        config::write_private(manifest, &files, false)?;
    }
    for entry in files {
        let bytes = fs::read(source.join(&entry.path))?;
        if digest_bytes(&bytes) != entry.sha256 {
            return Err(invalid("Project changed while staging."));
        }
        enumeray_connector::stage_setup_file(
            &destination.join(&entry.path),
            &bytes,
            entry.executable,
        )
        .map_err(invalid)?;
    }
    fs::File::open(destination)?.sync_all()?;
    Ok(())
}
pub(super) fn execute(
    args: &Args,
    options: &Setup,
    client: &EnumerayClient,
    directory: &Path,
    plan: &Plan,
    p: &mut super::Progress,
) -> Result<Completion, Failure> {
    let first = plan.input.first_model.as_ref().unwrap();
    if p.trial.complete {
        return complete(finish(directory, plan, p), 0);
    }
    let mut cursor = None::<String>;
    let mut selected = None;
    loop {
        let query = cursor
            .as_deref()
            .map(|c| vec![("cursor", c)])
            .unwrap_or_default();
        let page = client.management().available_compute_grants(&query)?;
        for grant in page.items {
            if grant.context_id == plan.input.host.context.id
                && Some(grant.id) == p.grant.as_ref().and_then(|g| g.id)
            {
                selected = Some(grant);
            }
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let grant=selected.ok_or_else(||invalid("The saved context grant is no longer available; setup will not select a replacement grant."))?;
    if grant.context.installation_roots != plan.input.host.context.definition.installation_roots {
        return Err(
            invalid("Approved installation roots changed; inspect the saved context.").into(),
        );
    }
    if p.trial.destination.is_none() {
        let destination = if let Some(path) = &first.installed_release {
            path.clone()
        } else {
            let root = if grant.context.installation_roots.len() == 1 || !interactive(options) {
                &grant.context.installation_roots[0]
            } else {
                eprintln!(
                    "Approved roots: {}",
                    grant.context.installation_roots.join(", ")
                );
                let chosen = prompt::ask(
                    "Approved installation root",
                    &grant.context.installation_roots[0],
                )?;
                grant
                    .context
                    .installation_roots
                    .iter()
                    .find(|r| **r == chosen)
                    .ok_or_else(|| invalid("Choose one of the context's approved roots."))?
            };
            Path::new(root)
                .join(&first.model_id)
                .join(format!("setup-{}", plan.operation_id))
        };
        if !grant
            .context
            .installation_roots
            .iter()
            .any(|r| destination.starts_with(r) && destination != Path::new(r))
        {
            return Err(invalid(
                "Installation must be a release directory under an approved context root.",
            )
            .into());
        }
        p.trial.destination = Some(destination);
        save(directory, p)?;
    }
    let source = if first.demo {
        let source = directory.join("starter");
        if !source.exists() {
            crate::starter::init(
                &first.model_id,
                &source,
                StarterKind::Python,
                StarterIo::Json,
                if grant.context.driver == "slurm" {
                    StarterDriver::Slurm
                } else {
                    StarterDriver::Process
                },
            )?;
        }
        source
    } else {
        first.project.clone().unwrap()
    };
    let (_, parsed) = crate::manifest::publication_source(&source.join("publication.json"))?;
    if parsed.model.id != first.model_id {
        return Err(
            invalid("Project model ID differs from the saved first-model selection.").into(),
        );
    }
    if first.demo && grant.context.driver == "slurm" && !p.trial.source_prepared {
        eprintln!(
            "Prepare {} for the selected Slurm site: allocation, partition, shared paths and scientific runtime. Setup does not provision them.",
            source.join("model.sbatch").display()
        );
        if !(options.confirm_model_preparation
            || (interactive(options)
                && prompt::yes(
                    "Has this starter's site-specific batch/runtime preparation been reviewed",
                    false,
                )?))
        {
            let mut value = summary(directory, plan, p, "scientific_preparation_required");
            value["source"] = json!(source);
            value["next_action"] = json!(
                "Prepare the generated starter for the selected Slurm site, then resume with --confirm-model-preparation. No candidate or trial has been registered."
            );
            return complete(value, 1);
        }
        p.trial.source_prepared = true;
        save(directory, p)?;
    }
    let destination = p.trial.destination.as_ref().unwrap().clone();
    let remote = grant.context.connection == "ssh";
    if first.installed_release.is_none()
        && !remote
        && let Err(error) = install(&source, &destination, &directory.join("installation.json"))
    {
        let mut v = summary(directory, plan, p, "installation_required");
        v["destination"] = json!(destination);
        v["source"] = json!(source);
        v["correction"] = json!(format!(
            "{error} Complete the approved installation as its execution account/site operator, then resume this same setup."
        ));
        return complete(v, 1);
    }
    let publication = directory.join("first-publication.json");
    if !publication.exists() {
        let (mut document, parsed) =
            crate::manifest::publication_source(&source.join("publication.json"))?;
        if parsed.model.id != first.model_id {
            return Err(
                invalid("Project model ID differs from the saved first-model selection.").into(),
            );
        }
        document["execution"]["context_id"] = json!(plan.input.host.context.id);
        document["execution"]["release_path"] = json!(destination);
        config::write_private(&publication, &document, false)?;
    }
    if remote {
        eprintln!(
            "Install the complete prepared project at {} on the selected SSH execution host using your site's approved route. Scientific runtime and Slurm setup remain site-owned.",
            destination.display()
        );
    }
    if !p.trial.model_registered {
        if !p.trial.model_started {
            match client.management().managed_model(&first.model_id) {
                Ok(_) => {
                    if !options.adopt_existing
                        && !(interactive(options)
                            && prompt::yes(
                                "Use the existing model for this candidate without changing its current publication",
                                false,
                            )?)
                    {
                        return Err(invalid("Model already exists; explicitly adopt it before creating the first trial candidate.").into());
                    }
                }
                Err(Error::Api { status: 404, .. }) => {}
                Err(e) => return Err(e.into()),
            }
            p.trial.model_started = true;
            save(directory, p)?;
        }
        crate::commands::model(
            client,
            &Model::Create {
                id: first.model_id.clone(),
                name: None,
                description: String::new(),
                file: Some(publication.clone()),
            },
        )?;
        p.trial.model_registered = true;
        save(directory, p)?;
    }
    let release = directory.join("first-release");
    if !release.exists() {
        crate::release::execute(
            args,
            client,
            &Release::Init {
                model: first.model_id.clone(),
                directory: release.clone(),
                file: Some(publication.clone()),
                rollback_from: None,
                candidate: None,
            },
        )?;
    }
    let prepared: Value = read(&publication)?;
    let publication_hash = hash(&prepared)?;
    if p.trial
        .publication_sha256
        .as_ref()
        .is_some_and(|original| original != &publication_hash)
    {
        return Err(invalid(
            "Prepared first-model publication changed; preserve the original release operation.",
        )
        .into());
    }
    p.trial.publication_sha256 = Some(publication_hash);
    save(directory, p)?;
    let saved: Value = read(&release.join("release.json"))?;
    let candidate = saved["candidate"]["id"]
        .as_str()
        .ok_or_else(|| invalid("Saved release lacks its candidate identity."))?
        .to_owned();
    if p.trial
        .candidate_id
        .as_ref()
        .is_some_and(|original| original != &candidate)
    {
        return Err(invalid(
            "Saved first candidate differs from this setup's retained registration identity.",
        )
        .into());
    }
    p.trial.candidate_id = Some(candidate.clone());
    save(directory, p)?;
    let (_, source) = crate::manifest::publication_source(&publication)?;
    let expected = source
        .candidate(candidate.clone())
        .map_err(|e| invalid(e.to_string()))?;
    if saved["model_id"] != first.model_id
        || !enumeray_contracts::numeric::equivalent(
            &saved["candidate"],
            &serde_json::to_value(expected)?,
        )
    {
        return Err(invalid("Saved first release differs from this setup's model, context, installation or scientific declaration; no registration was resumed.").into());
    }
    let result = crate::release::execute(
        args,
        client,
        &Release::Resume {
            directory: release.clone(),
            interval: 2,
            deadline: 300,
        },
    )?;
    if result.exit != 0 {
        return Ok(result);
    }
    let analysis = directory.join("first-trial");
    if !analysis.exists() {
        execute_analysis(
            args,
            client,
            Analysis::Init {
                model: first.model_id.clone(),
                candidate: Some(candidate.clone()),
                directory: analysis.clone(),
                example: first.example.clone(),
                inputs: first.inputs.clone(),
                file: first.files.clone(),
                collection: first.collections.clone(),
                collection_manifest: first.collection_manifests.clone(),
                media_type: first.media_types.clone(),
                metadata: first.metadata.clone(),
            },
            None,
        )?;
    }
    let key = crate::analysis::local_run_key(client, &analysis)?;
    if p.trial
        .run_key
        .as_ref()
        .is_some_and(|original| original != &key)
    {
        return Err(invalid(
            "Saved first trial retry identity changed; no replacement execution was authorized.",
        )
        .into());
    }
    p.trial.run_key = Some(key.clone());
    save(directory, p)?;
    let observed = execute_analysis(
        args,
        client,
        Analysis::Status {
            directory: analysis.clone(),
        },
        Some(&key),
    )?;
    let identity = observed
        .value
        .ok_or_else(|| invalid("Saved first trial returned no identity."))?;
    if identity["model_id"] != first.model_id
        || identity["publication_id"] != candidate
        || identity["purpose"] != "candidate_trial"
        || identity["context_id"] != plan.input.host.context.id
        || identity["grant_id"] != json!(p.grant.as_ref().and_then(|g| g.id))
    {
        return Err(invalid("Saved first trial belongs to another model, candidate, context or grant; no execution was submitted.").into());
    }
    if !p.trial_approved {
        if interactive(options) {
            let command = crate::args::Command::Analysis(Analysis::Status {
                directory: analysis.clone(),
            });
            let view = Args {
                api: args.api.clone(),
                profile: args.profile.clone(),
                config: args.config.clone(),
                json: false,
                timeout: args.timeout,
                command,
            };
            let crate::args::Command::Analysis(command) = &view.command else {
                unreachable!()
            };
            let observed = crate::analysis::execute(&view, client, command)?;
            if let Some(value) = observed.value {
                eprintln!("{}", crate::output::render(&view, &value));
            }
        }
        eprintln!(
            "Candidate {candidate} is ready. Review the saved first-trial inputs, execution destination and disclosure before submitting scientific code."
        );
        if !(options.submit_trial
            || (interactive(options) && prompt::yes("Submit this saved candidate trial", false)?))
        {
            let mut v = summary(directory, plan, p, "trial_review_required");
            v["analysis_directory"] = json!(analysis);
            return complete(v, 1);
        }
        p.trial_approved = true;
        save(directory, p)?;
    }
    execute_analysis(
        args,
        client,
        Analysis::Submit {
            directory: analysis.clone(),
        },
        Some(&key),
    )?;
    let result = execute_analysis(
        args,
        client,
        Analysis::Wait {
            directory: analysis.clone(),
            interval: 2,
            deadline: 300,
            check_outcome: true,
        },
        Some(&key),
    )?;
    if result.exit != 0 {
        return Ok(result);
    }
    execute_analysis(
        args,
        client,
        Analysis::Collect {
            directory: analysis,
            member: vec![],
            evidence_only: false,
        },
        Some(&key),
    )?;
    p.trial.complete = true;
    save(directory, p)?;
    complete(finish(directory, plan, p), 0)
}
fn finish(directory: &Path, plan: &Plan, p: &super::Progress) -> Value {
    let mut value = summary(directory, plan, p, "trial_complete");
    value["release_directory"] = json!(directory.join("first-release"));
    value["analysis_directory"] = json!(directory.join("first-trial"));
    value["next_action"] = json!(
        "Inspect collected trial evidence. Use model release review RELEASE_DIRECTORY, then explicitly model release activate RELEASE_DIRECTORY when scientifically appropriate. Setup does not activate publications."
    );
    value
}

fn execute_analysis(
    args: &Args,
    client: &EnumerayClient,
    command: Analysis,
    expected_key: Option<&str>,
) -> Result<Completion, Failure> {
    let forwarded = Args {
        api: args.api.clone(),
        profile: args.profile.clone(),
        config: args.config.clone(),
        json: args.json,
        timeout: args.timeout,
        command: crate::args::Command::Analysis(command),
    };
    let crate::args::Command::Analysis(command) = &forwarded.command else {
        unreachable!()
    };
    if let Some(key) = expected_key {
        crate::analysis::execute_bound(&forwarded, client, command, key)
    } else {
        crate::analysis::execute(&forwarded, client, command)
    }
}

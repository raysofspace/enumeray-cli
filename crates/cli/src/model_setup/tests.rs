use super::*;
use clap::Parser;
use std::io::Cursor;

fn state() -> (tempfile::TempDir, State, Args) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("setup");
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let state = State::new(root.clone());
    let args = Args::try_parse_from([
        "enumeray",
        "model",
        "setup",
        "--directory",
        root.to_str().unwrap(),
    ])
    .unwrap();
    (tmp, state, args)
}

fn scripted(inputs: &str) -> Prompt<Cursor<&[u8]>, Vec<u8>> {
    Prompt {
        input: Cursor::new(inputs.as_bytes()),
        output: Vec::new(),
    }
}

#[test]
fn offline_starter_creates_a_runnable_project_and_pauses_for_author_tests() {
    let (_tmp, mut state, args) = state();
    let mut prompt = scripted("starter\nlab-demo\npython\njson\nprocess\nno\n");
    let outcome = advance(&args, &mut state, &mut prompt)
        .map_err(|e| e.error)
        .unwrap();
    assert_eq!(outcome["stage"], "local_checks");
    assert_eq!(outcome["model_id"], "lab-demo");
    assert!(state.account.is_none());
    let project = state.project.as_ref().unwrap().parent().unwrap();
    assert!(project.join("test_starter.py").is_file());
    let output = String::from_utf8(prompt.output).unwrap();
    assert!(output.contains("python3 -B -m unittest"));
    assert!(!state.release().exists());
}

#[test]
fn offline_import_and_status_leave_the_declaration_and_checkpoint_unchanged() {
    let (_tmp, mut state, args) = state();
    let project = state.directory.join("existing");
    starter::init(
        "lab-existing",
        &project,
        StarterKind::Executable,
        StarterIo::Files,
        StarterDriver::Slurm,
    )
    .unwrap();
    let source = project.join("publication.json");
    let original = fs::read(&source).unwrap();
    let answers = format!("existing\n{}\nno\n", source.display());
    let outcome = advance(&args, &mut state, &mut scripted(&answers))
        .map_err(|e| e.error)
        .unwrap();
    assert_eq!(outcome["stage"], "local_checks");
    let checkpoint = fs::read(state.directory.join(STATE)).unwrap();
    let args = Args::try_parse_from([
        "enumeray",
        "--json",
        "model",
        "setup",
        "status",
        "--directory",
        state.directory.to_str().unwrap(),
    ])
    .unwrap();
    let crate::args::Command::Model(crate::args::Model::Setup(options)) = &args.command else {
        panic!("expected model setup command");
    };
    let inspected = execute(&args, options).map_err(|e| e.error).unwrap();
    assert_eq!(inspected.exit, 0);
    assert_eq!(inspected.value.unwrap()["stage"], "local_checks");
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(fs::read(state.directory.join(STATE)).unwrap(), checkpoint);
}

#[test]
fn file_reference_requires_a_criterion_before_submission() {
    let (_tmp, state, _args) = state();
    let project = state.directory.join("file-example");
    starter::init(
        "lab-file",
        &project,
        StarterKind::Executable,
        StarterIo::Files,
        StarterDriver::Process,
    )
    .unwrap();
    let (_, source) = manifest::publication_source(&project.join("publication.json")).unwrap();
    let mut prompt = scripted("\nTwo rows with reference CSV values within 0.01\n");
    let reference = choose_reference(&source, &mut prompt).unwrap().unwrap();
    assert_eq!(reference.example, "starter-example");
    assert_eq!(
        reference.criterion.as_deref(),
        Some("Two rows with reference CSV values within 0.01")
    );
}

fn example() -> ScientificExample {
    ScientificExample {
        name: "small".into(),
        inputs: json!({"value":2}),
        outputs: Some(json!({"doubled":4})),
    }
}

#[test]
fn exact_reference_requires_the_frozen_input_and_matching_result() {
    let mut trial =
        json!({"run":{"effective_inputs":{"value":2.0},"result":{"outputs":{"doubled":4.0}}}});
    assert!(exact_result(&example(), &trial));
    trial["run"]["effective_inputs"]["value"] = json!(5);
    assert!(!exact_result(&example(), &trial));
    trial["run"]["effective_inputs"]["value"] = json!(2);
    trial["run"]["result"]["outputs"]["doubled"] = json!(4.001);
    assert!(!exact_result(&example(), &trial));
}

#[test]
fn accepted_trial_must_match_the_approved_review() {
    let (_tmp, mut state, _args) = state();
    let approved =
        json!({"example":"small","inputs":{"value":2},"execution_location":"Lab","input_files":[]});
    state.trial_approval = Some(digest(&approved).unwrap());
    let mut frozen = approved.clone();
    frozen.as_object_mut().unwrap().remove("example");
    let mut trial = json!({"frozen_review":frozen});
    require_trial_approval(&state, &trial, &example()).unwrap();
    trial["frozen_review"]["execution_location"] = json!("Somewhere else");
    assert!(require_trial_approval(&state, &trial, &example()).is_err());
}

#[test]
fn stale_review_has_no_activation_command() {
    let (_tmp, mut state, args) = state();
    let trial = json!({"run_id":"run-id", "collection":{"complete":true}});
    let snapshot = json!({"candidate":{"id":"candidate"},"expected_current":null});
    state.review_digest = Some(digest(&snapshot).unwrap());
    let stale = ready_view(
        &args,
        &state,
        "lab-test",
        &trial,
        &json!({"stage":"ready","guard_changed":true}),
    )
    .unwrap();
    assert_eq!(stale["stage"], "release_review");
    assert!(stale["activation_command"].is_null());
    let current = ready_view(
        &args,
        &state,
        "lab-test",
        &trial,
        &json!({"stage":"reviewed","guard_changed":false,"review_snapshot":snapshot}),
    )
    .unwrap();
    assert_eq!(current["stage"], "ready_for_activation");
    assert!(
        current["activation_command"]
            .as_str()
            .unwrap()
            .contains("model release activate")
    );
}

//! An extension written in C (`examples/c/notes.c`), compiled by the build script where
//! there is a C compiler, linked in and (on Unix) also loaded from a shared library, doing what a Rust extension does: through the harness's public API, with no network
//! and no model but the one the C code is.

#![cfg(all(c_example, feature = "harness"))]
#![expect(
    unsafe_code,
    reason = "naming a foreign symbol is unsafe wherever it is done: see docs/c-extensions.md"
)]

use std::sync::Arc;

use corduroy_cabi::abi::Extension;
use corduroy_cabi::{CExtension, LoadError};
use corduroy_core::{
    CallError, Draft, FiberState, PROMPTS, Pattern, Prompts, Source, TOOL_CALL, ToolError, Verdict,
};
use corduroy_harness::scripted::ScriptedProvider;
use corduroy_harness::{Harness, HarnessError, Status};

// SAFETY: both are defined in `examples/c/notes.c` with this signature.
unsafe extern "C" {
    fn corduroy_extension_notes(extension: *mut Extension) -> i32;
    fn corduroy_extension_careless(extension: *mut Extension) -> i32;
}

fn notes() -> Result<CExtension, LoadError> {
    // SAFETY: `notes.c` is written to the header's rules.
    unsafe { CExtension::load(corduroy_extension_notes) }
}

fn harness() -> Result<Harness, HarnessError> {
    Harness::new(Arc::new(ScriptedProvider::new([])))
}

#[test]
fn a_c_extension_contributes_one_of_each_and_all_of_it_is_taken_back() {
    let harness = harness().unwrap();
    let id = harness.plugin(notes().unwrap());
    assert_eq!(
        harness.with_app(|app| app.state(id).cloned()),
        Some(FiberState::Active)
    );

    // A tool, and its failures as data.
    let tools = harness.tools();
    assert_eq!(
        tools.call("note", r#"{"text":"milk"}"#).unwrap().to_text(),
        "noted (1)"
    );
    assert_eq!(
        tools.call("note", "{}"),
        Err(ToolError::InvalidInput("a note needs `text`".into()))
    );

    // A gate, with an opinion only where it has one.
    let denied = harness
        .gates()
        .decide(TOOL_CALL, r#"{"tool":"note","input":{"text":"forbidden"}}"#)
        .unwrap();
    assert_eq!(denied.gate, "notes-gate");
    assert_eq!(
        denied.verdict,
        Verdict::Deny("that word is not to be written down".into())
    );
    assert!(harness.gates().decide(TOOL_CALL, "{}").is_none());

    // A part of the prompt, a secret and a service.
    let prompts = harness
        .with_app(|app| app.service::<Prompts>(PROMPTS))
        .unwrap();
    assert_eq!(
        prompts.render(),
        [("notes".to_owned(), "You have taken 1 notes.".to_owned())]
    );
    assert_eq!(harness.secrets().reveal("notes-key").unwrap(), "s3cret");
    let services = harness.services();
    assert_eq!(services.call("test", "notes", "count", "").unwrap(), "1");
    assert!(matches!(
        services.call("test", "notes", "erase", ""),
        Err(CallError::Unknown(_))
    ));

    // A listener.
    harness
        .events()
        .emit(Draft::new(Source::Environment, "tool/result").payload("{}"))
        .unwrap();
    assert_eq!(services.call("test", "notes", "heard", "").unwrap(), "1");

    // Unloading takes back everything, the provider it loaded as a child included, and
    // runs the effect's undoing, which emits through the handle.
    assert!(harness.providers().get("c-model").is_some());
    harness.with_app(|app| app.dispose(id)).unwrap();
    assert!(tools.get("note").is_none());
    assert_eq!(harness.gates().names(), Vec::<String>::new());
    assert_eq!(prompts.render(), Vec::new());
    assert!(harness.secrets().get("notes-key").is_none());
    assert!(services.get("notes").is_none());
    assert!(harness.providers().get("c-model").is_none());
    let closed = harness.events().matching(&Pattern::new("notes/closed"));
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].source, Source::Plugin("notes".into()));
}

#[tokio::test]
async fn the_loop_asks_a_model_written_in_c_which_answers_from_its_own_thread() {
    let harness = harness().unwrap().with_provider("c-model");
    harness.plugin(notes().unwrap());

    let outcome = harness.run("take a note").await.unwrap();

    assert_eq!(
        outcome.status,
        Status::Finished {
            answer: "there are 1 notes".into()
        }
    );
    let results = harness.events().matching(&Pattern::new("tool/result"));
    assert_eq!(results.len(), 1);
    assert!(results[0].payload.contains("noted (1)"));
}

#[test]
fn a_registration_that_fails_fails_apply_even_if_the_extension_did_not_look() {
    let harness = harness().unwrap();
    // SAFETY: `notes.c` is written to the header's rules.
    let careless = unsafe { CExtension::load(corduroy_extension_careless) }.unwrap();
    let id = harness.plugin(careless);

    let state = harness.with_app(|app| app.state(id).cloned());

    let Some(FiberState::Failed(why)) = state else {
        panic!("expected a failed plugin, got {state:?}");
    };
    assert!(why.contains("gates"), "{why}");
    assert_eq!(harness.gates().names(), Vec::<String>::new());
}

#[cfg(all(unix, feature = "load"))]
#[test]
fn an_extension_is_taken_from_a_shared_library_when_the_program_runs() {
    use std::path::Path;

    let library = Path::new(env!("CORDUROY_NOTES_LIBRARY"));
    let harness = harness().unwrap();
    // SAFETY: the library is `notes.c`, built by this crate's build script.
    let loaded = unsafe { CExtension::open(library, "corduroy_extension_notes") }.unwrap();
    let id = harness.plugin(loaded);

    assert_eq!(
        harness
            .tools()
            .call("note", r#"{"text":"loaded"}"#)
            .unwrap()
            .to_text(),
        "noted (1)"
    );
    // Its code stays where it is; what it registered does not.
    harness.with_app(|app| app.dispose(id)).unwrap();
    assert!(harness.tools().get("note").is_none());

    // SAFETY: nothing is loaded from a file that is not there, or a name that is not.
    let missing = unsafe { CExtension::open(Path::new("/nowhere/notes.so"), "x") };
    assert!(missing.is_err());
    // SAFETY: as above.
    let unnamed = unsafe { CExtension::open(library, "corduroy_extension_absent") };
    assert!(unnamed.is_err());
}

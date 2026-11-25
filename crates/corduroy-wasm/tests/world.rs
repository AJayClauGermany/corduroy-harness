//! Which versions of the plugin world the host loads. An add-on built against another
//! version is refused when it is loaded, before anything is instantiated, in the host's own
//! words. The add-ons here are made from the world's WIT with its version changed: components
//! that import and export all of the world and do nothing.

use std::error::Error;
use std::sync::Arc;

use corduroy_core::{
    App, FiberState, GATES, Gates, HTTP, HttpClients, KERNELS, Kernels, MODELS, Models, PROMPTS,
    Prompts, SECRETS, SERVICES, Scope, Secrets, Services, TOOLS, Tools,
};
use corduroy_wasm::{CAPABILITIES, WORLD, WasmError, WasmRuntime};
use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
use wit_parser::{ManglingAndAbi, Resolve};

/// An add-on built against the world as version `version` of the package names it.
fn built_for(version: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let (_, ours) = WORLD.split_once('@').ok_or("the world has a version")?;
    let wit = include_str!("../../../wit/corduroy.wit");
    let wit = wit.replacen(&format!("@{ours};"), &format!("@{version};"), 1);
    let mut resolve = Resolve::default();
    let package = resolve.push_str("corduroy.wit", &wit)?;
    let world = resolve.select_world(&[package], Some("plugin"))?;
    let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
    embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8, false)?;
    Ok(ComponentEncoder::default()
        .validate(true)
        .module(&module)?
        .encode()?)
}

/// Why the host refuses the add-on built for `version`.
fn refusal(version: &str) -> Result<String, Box<dyn Error>> {
    match WasmRuntime::new()?.load_bytes("other", &built_for(version)?) {
        Err(WasmError::Invalid(why)) => Ok(why),
        Err(other) => Err(other.into()),
        Ok(_) => Err(format!("an add-on built for {version} was loaded").into()),
    }
}

#[test]
fn an_add_on_built_for_this_host_s_world_loads() {
    let (_, ours) = WORLD.split_once('@').unwrap();

    let plugin = WasmRuntime::new()
        .unwrap()
        .load_bytes("same", &built_for(ours).unwrap())
        .unwrap();

    // The whole world, so every capability: the check reads what the add-on imports.
    let mut required = plugin.requires().to_vec();
    required.sort();
    let mut all = CAPABILITIES.map(str::to_owned).to_vec();
    all.sort();
    assert_eq!(required, all);
}

#[test]
fn an_add_on_built_for_another_version_is_refused_at_load_naming_both() {
    for other in ["0.2.0", "1.0.0"] {
        let why = refusal(other).unwrap();

        assert!(
            why.contains(&format!(
                "built for the plugin world corduroy:plugin@{other}"
            )),
            "{why}"
        );
        assert!(why.contains(&format!("this host's is {WORLD}")), "{why}");
        assert!(why.contains("Build it again"), "{why}");
    }
}

#[test]
fn a_patch_version_changes_no_shape_so_it_loads_and_links() {
    let mut plugin = WasmRuntime::new()
        .unwrap()
        .load_bytes("patched", &built_for("0.1.7").unwrap())
        .unwrap();
    for capability in CAPABILITIES {
        plugin = plugin.grant(capability, Scope::none());
    }
    let mut app = App::new();
    app.provide(TOOLS, Arc::new(Tools::new())).unwrap();
    app.provide(PROMPTS, Arc::new(Prompts::new())).unwrap();
    app.provide(GATES, Arc::new(Gates::new())).unwrap();
    app.provide(SERVICES, Arc::new(Services::new())).unwrap();
    app.provide(KERNELS, Arc::new(Kernels::new())).unwrap();
    app.provide(MODELS, Arc::new(Models::new())).unwrap();
    app.provide(SECRETS, Arc::new(Secrets::new())).unwrap();
    app.provide(HTTP, Arc::new(HttpClients::new())).unwrap();

    let started = app.plugin(plugin);

    // It does nothing, so its `activate` traps: it was linked and instantiated to get there.
    let FiberState::Failed(why) = app.state(started).unwrap() else {
        panic!("an add-on that does nothing started");
    };
    assert!(why.contains("trapped"), "{why}");
}

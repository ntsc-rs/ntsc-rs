//! Update a macOS GStreamer .pkg's `choices.xml` in place, selecting only the comma-separated GStreamer components
//! specified as an argument.
//!
//! This allows us to exclude GStreamer components that ntsc-rs doesn't use.

use std::collections::BTreeSet;
use std::error::Error;
use std::path::PathBuf;

use clap::builder::PathBufValueParser;

pub fn command() -> clap::Command {
    clap::Command::new("macos-gst-components")
        .about("Update a macOS GStreamer .pkg's `choices.xml` in place.")
        .arg(
            clap::Arg::new("components")
                .long("components")
                .help("The GStreamer components to enable, as a comma-separated list.")
                .value_delimiter(',')
                .required(true),
        )
        .arg(
            clap::Arg::new("choices-file")
                .help("Path to the choices.xml file.")
                .value_parser(PathBufValueParser::new())
                .required(true),
        )
}

pub fn main(args: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let mut components: BTreeSet<&str> = args
        .get_many::<String>("components")
        .unwrap()
        .map(|c| c.as_str())
        .collect::<BTreeSet<_>>();
    let choices_file = args.get_one::<PathBuf>("choices-file").unwrap();

    let mut choices = plist::Value::from_file(choices_file)?;
    let choices_arr = choices.as_array_mut().ok_or("unexpected plist format")?;
    for choice in choices_arr {
        let Some(choice_dict) = choice.as_dictionary_mut() else {
            continue;
        };

        if choice_dict
            .get("choiceAttribute")
            .and_then(|v| v.as_string())
            != Some("selected")
        {
            continue;
        }

        let Some(choice_identifier) = choice_dict
            .get("choiceIdentifier")
            .and_then(|v| v.as_string())
        else {
            eprintln!("found a choice with no choiceIdentifier");
            continue;
        };

        let Some(suffix) = choice_identifier.strip_prefix("org.freedesktop.gstreamer.darwin.")
        else {
            continue;
        };

        // The development package uses the same component names as the runtime package,
        // but appends `-devel` to every choice identifier.
        let component = suffix.strip_suffix("-devel").unwrap_or(suffix);

        if !component.starts_with("gstreamer-1.0-") {
            // We don't want to disable any components that aren't GStreamer...bundles?
            continue;
        }

        // This was in the list of components to *keep*.
        let should_keep = components.remove(component);

        let setting = choice_dict
            .get_mut("attributeSetting")
            .expect("no attributeSetting present");
        *setting = plist::Value::Integer((should_keep as u8).into());
    }

    for remaining_component in components {
        eprintln!("warning: component {remaining_component} not found in choices.xml");
    }

    choices.to_file_xml(choices_file)?;

    Ok(())
}

use super::{emit, envelope, recorded_text};
use serde_json::{Value, json};
use std::process::ExitCode;

pub(crate) fn tools(report: &crate::application::tools::Report) -> ExitCode {
    let mut result = envelope("tool", "success", Some(&report.outcome));
    let mut truncated = Vec::new();
    let tools = report.tools.iter().enumerate().map(|(index, info)| {
        let base = format!("/data/tools/{index}");
        let commands = info.tool.commands.iter().enumerate().map(|(command_index, (name, bin))| {
            let path = format!("{base}/commands/{command_index}");
            json!({"name":recorded_text(name, &format!("{path}/name"), &mut truncated),
                "target":recorded_text(bin.target.display(), &format!("{path}/target"), &mut truncated)})
        }).collect::<Vec<_>>();
        let mut field = |value: &dyn std::fmt::Display, key: &str| recorded_text(value, &format!("{base}/{key}"), &mut truncated);
        json!({
            "name":field(&info.name,"name"), "version":field(&info.tool.version,"version"),
            "requirement":field(&info.tool.requirement,"requirement"),
            "environment":field(&info.environment.display(),"environment"),
            "lockfile":field(&info.environment.join("tapid.lock").display(),"lockfile"),
            "commands": commands
        })
    }).collect::<Vec<_>>();
    result["data"] = json!({"tools":tools, "bin_dir":recorded_text(report.bin_dir.display(), "/data/bin_dir", &mut truncated)});
    let fields = result["truncated_fields"].as_array_mut().unwrap();
    fields.extend(truncated.into_iter().map(Value::String));
    fields.sort_unstable_by(|a, b| a.as_str().cmp(&b.as_str()));
    emit(result, 0)
}

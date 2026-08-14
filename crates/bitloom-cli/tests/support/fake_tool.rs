use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs, process, thread};

fn main() {
    let executable = env::current_exe().expect("fake tool executable path");
    let tool = executable
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("UTF-8 fake tool name")
        .to_ascii_lowercase();
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    let state = PathBuf::from(env::var_os("BITLOOM_FAKE_STATE").expect("fake state directory"));
    fs::create_dir_all(&state).expect("create fake state directory");

    println!("{tool} fake stdout");
    eprintln!("{tool} fake stderr");

    match tool.as_str() {
        "gdk-pixbuf-query-loaders" => mark(&state, "gdk.call", &arguments),
        "yosys" => fake_yosys(&state, &arguments),
        "nextpnr-himbaechel" => fake_nextpnr(&state, &arguments),
        "gowin_pack" => fake_pack(&state, &arguments),
        "openfpgaloader" => fake_programmer(&state, &arguments),
        name => panic!("unexpected fake tool name: {name}"),
    }
}

fn fake_yosys(state: &Path, arguments: &[std::ffi::OsString]) {
    mark(state, "yosys.call", arguments);
    let script = argument_after(arguments, "-s");
    let script = fs::read_to_string(script).expect("read Yosys script");
    let command =
        script.lines().find(|line| line.starts_with("synth_gowin ")).expect("synth_gowin command");
    let output =
        command.split_once(" -json ").map(|(_, value)| unquote(value)).expect("Yosys JSON output");
    write(&output, b"fake synthesized netlist");
    maybe_sleep("yosys");
    maybe_fail("yosys", "ERROR: synthetic synthesis failure");
}

fn fake_nextpnr(state: &Path, arguments: &[std::ffi::OsString]) {
    let seed = argument_after(arguments, "--seed").to_string_lossy();
    mark(state, &format!("nextpnr-{seed}.call"), arguments);
    let output = argument_after(arguments, "--write");
    let report = argument_after(arguments, "--report");
    write(Path::new(output), format!("fake routed netlist for seed {seed}").as_bytes());
    write(
        Path::new(report),
        format!(
            r#"{{"fmax":{{"clk":{{"achieved":{},"constraint":27.0}}}},"utilization":{{"LUT4":{{"used":42,"available":20736}}}}}}"#,
            100 + seed.parse::<u64>().expect("numeric seed")
        )
        .as_bytes(),
    );
    maybe_sleep("nextpnr");
    if env::var("BITLOOM_FAKE_FAIL").as_deref() == Ok("nextpnr-fatal") {
        eprintln!("ERROR: synthetic deterministic PNR failure");
        process::exit(1);
    }
}

fn fake_pack(state: &Path, arguments: &[std::ffi::OsString]) {
    mark(state, "gowin_pack.call", arguments);
    let output = argument_after(arguments, "--output");
    write(Path::new(output), b"fake bitstream");
    maybe_sleep("gowin_pack");
    maybe_fail("gowin_pack", "ERROR: synthetic packing failure");
}

fn fake_programmer(state: &Path, arguments: &[std::ffi::OsString]) {
    mark(state, "openfpgaloader.call", arguments);
    maybe_sleep("openfpgaloader");
    maybe_fail("openfpgaloader", "ERROR: synthetic programmer failure");
}

fn argument_after<'a>(arguments: &'a [std::ffi::OsString], name: &str) -> &'a OsStr {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_os_str())
        .unwrap_or_else(|| panic!("missing fake tool argument {name}"))
}

fn unquote(value: &str) -> PathBuf {
    PathBuf::from(value.trim().trim_matches('"').replace("\\\"", "\""))
}

fn write(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fake output parent");
    }
    fs::write(path, contents).expect("write fake tool output");
}

fn mark(state: &Path, name: &str, arguments: &[std::ffi::OsString]) {
    let contents =
        arguments.iter().map(|argument| argument.to_string_lossy()).collect::<Vec<_>>().join("\n");
    fs::write(state.join(name), contents).expect("write fake invocation marker");
}

fn maybe_sleep(tool: &str) {
    if env::var("BITLOOM_FAKE_SLEEP").as_deref() == Ok(tool) {
        thread::sleep(Duration::from_secs(60));
    }
}

fn maybe_fail(tool: &str, message: &str) {
    if env::var("BITLOOM_FAKE_FAIL").as_deref() == Ok(tool) {
        eprintln!("{message}");
        process::exit(1);
    }
}

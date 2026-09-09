use std::{env, fs, path::PathBuf};

#[allow(missing_docs)]
pub fn generate(specs: &[(&str, &str, bool)]) {
    let mut generated = String::new();
    for &(path, name, ili_prefix) in specs {
        println!("cargo:rerun-if-changed={path}");
        let source = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {path}: {error}"));
        let commands = parse_commands(&source);
        generated.push_str(&format!(
            "/// Controller initialization sequence generated from the vendor table.\n\
             pub static {name}: &[Command] = &[\n"
        ));
        if ili_prefix {
            generated.push_str(
                "    Command { command: 0x11, data: &[], delay_ms: 120 },\n\
                 Command { command: 0x36, data: &[0x00], delay_ms: 0 },\n\
                 Command { command: 0x3a, data: &[0x55], delay_ms: 0 },\n",
            );
        }
        for command in commands {
            generated.push_str(&format!(
                "    Command {{ command: 0x{:02x}, data: &{:?}, delay_ms: {} }},\n",
                command.command, command.data, command.delay_ms
            ));
        }
        generated.push_str("];\n\n");
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"))
        .join("panel_commands.rs");
    fs::write(output, generated).expect("write generated panel commands");
}

struct Command {
    command: u8,
    data: Vec<u8>,
    delay_ms: u16,
}

fn parse_commands(source: &str) -> Vec<Command> {
    let source = strip_comments(source);
    let start = source.find("[] = {").expect("display command array") + 6;
    let end = source
        .rfind("};")
        .expect("display command array terminator");
    split_entries(&source[start..end])
        .into_iter()
        .map(parse_command)
        .collect()
}

fn strip_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if bytes[index..].starts_with(b"/*") {
            index += 2;
            while index + 1 < bytes.len() && !bytes[index..].starts_with(b"*/") {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else {
            output.push(char::from(bytes[index]));
            index += 1;
        }
    }
    output
}

fn split_entries(body: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    for (index, byte) in body.bytes().enumerate() {
        match byte {
            b'{' => {
                if depth == 0 {
                    start = Some(index + 1);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.checked_sub(1).expect("balanced command braces");
                if depth == 0 {
                    entries.push(&body[start.expect("entry start")..index]);
                    start = None;
                }
            }
            _ => {}
        }
    }
    assert_eq!(depth, 0, "balanced display command array");
    entries
}

fn parse_command(entry: &str) -> Command {
    let fields = split_fields(entry);
    assert_eq!(fields.len(), 4, "four display command fields: {entry}");
    let command = parse_number(fields[0]) as u8;
    let mut data = if fields[1].trim() == "NULL" {
        Vec::new()
    } else {
        let data_start = fields[1].find('{').expect("command data start") + 1;
        let data_end = fields[1].rfind('}').expect("command data end");
        fields[1][data_start..data_end]
            .split(',')
            .filter(|value| !value.trim().is_empty())
            .map(|value| parse_number(value) as u8)
            .collect()
    };
    let declared_len = parse_number(fields[2]) as usize;
    if data.len() < declared_len {
        println!(
            "cargo:warning=panel command 0x{command:02x} declares {declared_len} bytes but contains {}; using the memory-safe parsed length",
            data.len()
        );
    }
    data.truncate(declared_len);
    Command {
        command,
        data,
        delay_ms: parse_number(fields[3]) as u16,
    }
}

fn split_fields(entry: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut braces = 0usize;
    let mut parentheses = 0usize;
    let mut start = 0usize;
    for (index, byte) in entry.bytes().enumerate() {
        match byte {
            b'{' => braces += 1,
            b'}' => braces -= 1,
            b'(' => parentheses += 1,
            b')' => parentheses -= 1,
            b',' if braces == 0 && parentheses == 0 => {
                fields.push(entry[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    fields.push(entry[start..].trim());
    fields
}

fn parse_number(value: &str) -> u64 {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).expect("hex display command value")
    } else {
        value.parse().expect("decimal display command value")
    }
}

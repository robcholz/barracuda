//! Generates the compact trust-root bundle, and compiles the C library
//! functions mbedTLS calls on bare-metal targets.

use std::{env, error::Error, fs, path::PathBuf};

/// The copied musl sources and Barracuda's wrappers, relative to `c/`.
const SOURCES: [&str; 7] = [
    "musl/memchr.c",
    "musl/strchr.c",
    "musl/strchrnul.c",
    "musl/strcmp.c",
    "musl/strstr.c",
    "musl/vfprintf.c",
    "runtime.c",
];

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest dir")?);
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing out dir")?);

    let roots = manifest.join("roots/cacert.pem");
    println!("cargo:rerun-if-changed={}", roots.display());
    fs::write(
        out.join("roots.bin"),
        bundle::generate(&fs::read_to_string(roots)?)?,
    )?;

    let c = manifest.join("c");
    println!("cargo:rerun-if-changed={}", c.display());
    // Host targets link their system C library.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return Ok(());
    }
    cc::Build::new()
        .std("c11")
        // musl is written for its own warning flags, not -Wall -Wextra.
        .warnings(false)
        .extra_warnings(false)
        // These headers come before the compiler's own.
        .include(c.join("include"))
        .include(c.join("musl"))
        .flag("-include")
        .flag(c.join("weak.h").to_string_lossy().as_ref())
        .files(SOURCES.map(|name| c.join(name)))
        .try_compile("tls_c_runtime")?;
    Ok(())
}

/// The compact bundle: for each root still valid on the store's date, its
/// subject name and public key, both as DER, sorted by name. ESP-IDF keeps the
/// same two fields per root; the issuer lookup and signature check need no more.
///
/// Layout, integers little-endian:
///
/// ```text
/// u16 count
/// count × { u16 name_len, u16 key_len, name, key }
/// ```
mod bundle {
    use std::error::Error;

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    pub fn generate(pem: &str) -> Result<Vec<u8>> {
        let as_of = store_date(pem)?;
        let mut roots = Vec::new();
        for der in certificates(pem)? {
            let root = parse(&der)?;
            if root.not_after >= as_of {
                roots.push((root.subject.to_vec(), root.key.to_vec()));
            }
        }
        roots.sort();
        roots.dedup();

        let mut bundle = Vec::new();
        bundle.extend_from_slice(&u16::try_from(roots.len())?.to_le_bytes());
        for (name, key) in &roots {
            bundle.extend_from_slice(&u16::try_from(name.len())?.to_le_bytes());
            bundle.extend_from_slice(&u16::try_from(key.len())?.to_le_bytes());
            bundle.extend_from_slice(name);
            bundle.extend_from_slice(key);
        }
        Ok(bundle)
    }

    /// The date of Mozilla's data, from curl's header, as `YYYYMMDDhhmmss`:
    /// roots that expired before it are left out.
    fn store_date(pem: &str) -> Result<u64> {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let line = pem
            .lines()
            .find_map(|line| line.strip_prefix("## Certificate data from Mozilla as of: "))
            .ok_or("cacert.pem has no Mozilla date header")?;
        // `Fri Sep 25 03:12:01 2026 GMT`
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [_, month, day, time, year, ..] = fields[..] else {
            return Err(format!("unexpected Mozilla date `{line}`").into());
        };
        let month = MONTHS
            .iter()
            .position(|name| *name == month)
            .ok_or("unexpected month")?
            + 1;
        let time = time.replace(':', "");
        Ok(format!("{year}{month:02}{day:0>2}{time}").parse()?)
    }

    fn certificates(pem: &str) -> Result<Vec<Vec<u8>>> {
        let mut certificates = Vec::new();
        let mut body: Option<String> = None;
        for line in pem.lines() {
            match line.trim() {
                "-----BEGIN CERTIFICATE-----" => body = Some(String::new()),
                "-----END CERTIFICATE-----" => {
                    let text = body.take().ok_or("END without BEGIN")?;
                    certificates.push(base64(&text)?);
                }
                text => {
                    if let Some(body) = body.as_mut() {
                        body.push_str(text);
                    }
                }
            }
        }
        Ok(certificates)
    }

    fn base64(text: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let (mut buffer, mut bits) = (0_u32, 0_u32);
        for symbol in text.bytes().filter(|byte| *byte != b'=') {
            let value = match symbol {
                b'A'..=b'Z' => symbol - b'A',
                b'a'..=b'z' => symbol - b'a' + 26,
                b'0'..=b'9' => symbol - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => return Err(format!("invalid base64 byte {symbol:#x}").into()),
            };
            buffer = (buffer << 6) | u32::from(value);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                bytes.push((buffer >> bits) as u8);
                buffer &= (1 << bits) - 1;
            }
        }
        Ok(bytes)
    }

    /// One DER element and the bytes after it.
    type Split<'a> = Result<(&'a [u8], &'a [u8])>;

    pub struct Root<'a> {
        pub subject: &'a [u8],
        pub key: &'a [u8],
        pub not_after: u64,
    }

    /// Reads the fields the bundle keeps from one DER certificate.
    pub fn parse(der: &[u8]) -> Result<Root<'_>> {
        let (certificate, _) = element(der, 0x30)?;
        let (tbs, _) = element(content(certificate)?, 0x30)?;
        let mut fields = content(tbs)?;
        // version [0] EXPLICIT, optional
        if fields.first() == Some(&0xa0) {
            fields = element(fields, 0xa0)?.1;
        }
        let (_serial, rest) = element(fields, 0x02)?;
        let (_signature, rest) = element(rest, 0x30)?;
        let (_issuer, rest) = element(rest, 0x30)?;
        let (validity, rest) = element(rest, 0x30)?;
        let (subject, rest) = element(rest, 0x30)?;
        let (key, _) = element(rest, 0x30)?;

        let (_not_before, rest) = any_element(content(validity)?)?;
        let (not_after, _) = any_element(rest)?;
        Ok(Root {
            subject,
            key,
            not_after: time(not_after)?,
        })
    }

    /// `YYYYMMDDhhmmss` from a UTCTime or GeneralizedTime element.
    fn time(element: &[u8]) -> Result<u64> {
        let text = std::str::from_utf8(content(element)?)?
            .strip_suffix('Z')
            .ok_or("time is not UTC")?;
        let full = match element.first() {
            Some(0x17) => {
                let year: u32 = text.get(..2).ok_or("short UTCTime")?.parse()?;
                let century = if year >= 50 { "19" } else { "20" };
                format!("{century}{text}")
            }
            Some(0x18) => text.to_owned(),
            _ => return Err("validity time has an unexpected type".into()),
        };
        Ok(full.parse()?)
    }

    /// Splits off one element with the expected tag: (whole element, rest).
    fn element(input: &[u8], tag: u8) -> Split<'_> {
        if input.first() != Some(&tag) {
            return Err(format!("expected DER tag {tag:#x}").into());
        }
        any_element(input)
    }

    fn any_element(input: &[u8]) -> Split<'_> {
        let (header, length) = header(input)?;
        let end = header.checked_add(length).ok_or("DER length overflow")?;
        if end > input.len() {
            return Err("DER element runs past its parent".into());
        }
        Ok(input.split_at(end))
    }

    fn content(element: &[u8]) -> Result<&[u8]> {
        let (header, length) = header(element)?;
        element
            .get(header..header + length)
            .ok_or_else(|| "truncated DER element".into())
    }

    /// (header length, content length) of the element at the start of `input`.
    fn header(input: &[u8]) -> Result<(usize, usize)> {
        let first = *input.get(1).ok_or("truncated DER header")?;
        if first < 0x80 {
            return Ok((2, usize::from(first)));
        }
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return Err("unsupported DER length".into());
        }
        let bytes = input.get(2..2 + count).ok_or("truncated DER length")?;
        let length = bytes
            .iter()
            .fold(0_usize, |length, byte| (length << 8) | usize::from(*byte));
        Ok((2 + count, length))
    }
}

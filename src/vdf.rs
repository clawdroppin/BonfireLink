//! Valve's KeyValues formats: the text form (`libraryfolders.vdf`, `localconfig.vdf`, `appmanifest_*.acf`)
//! and the binary form (`shortcuts.vdf`). Both round-trip everything they don't understand,
//! because Steam's files hold far more than the few keys we edit.

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Obj(Obj),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Obj(pub Vec<(String, Value)>);

impl Obj {
    /// Case-insensitive lookup, like Steam itself.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }
    pub fn obj(&self, key: &str) -> Option<&Obj> {
        match self.get(key) {
            Some(Value::Obj(o)) => Some(o),
            _ => None,
        }
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }
    /// Walks (and creates) nested objects along `path`.
    pub fn obj_path_mut(&mut self, path: &[&str]) -> &mut Obj {
        let mut cur = self;
        for key in path {
            let idx = match cur.0.iter().position(|(k, _)| k.eq_ignore_ascii_case(key)) {
                Some(i) if matches!(cur.0[i].1, Value::Obj(_)) => i,
                Some(i) => {
                    cur.0[i].1 = Value::Obj(Obj::default());
                    i
                }
                None => {
                    cur.0.push((key.to_string(), Value::Obj(Obj::default())));
                    cur.0.len() - 1
                }
            };
            cur = match &mut cur.0[idx].1 {
                Value::Obj(o) => o,
                Value::Str(_) => unreachable!(),
            };
        }
        cur
    }
    pub fn obj_path(&self, path: &[&str]) -> Option<&Obj> {
        let mut cur = self;
        for key in path {
            cur = cur.obj(key)?;
        }
        Some(cur)
    }
    pub fn set_str(&mut self, key: &str, value: &str) {
        match self.get_mut(key) {
            Some(v) => *v = Value::Str(value.to_string()),
            None => self.0.push((key.to_string(), Value::Str(value.to_string()))),
        }
    }
    pub fn remove(&mut self, key: &str) {
        self.0.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
    }
}

// ---------- text ----------

pub fn parse_text(src: &str) -> Result<Obj> {
    let mut p = Parser { s: src.as_bytes(), i: 0 };
    let root = p.obj(true)?;
    Ok(root)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

enum Tok {
    Str(String),
    Open,
    Close,
    Eof,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
                continue;
            }
            // Conditionals like [$WIN32] are attached to the previous key; we ignore them.
            if self.i < self.s.len() && self.s[self.i] == b'[' {
                while self.i < self.s.len() && self.s[self.i] != b']' {
                    self.i += 1;
                }
                self.i = (self.i + 1).min(self.s.len());
                continue;
            }
            break;
        }
    }

    fn tok(&mut self) -> Result<Tok> {
        self.skip_ws();
        if self.i >= self.s.len() {
            return Ok(Tok::Eof);
        }
        match self.s[self.i] {
            b'{' => {
                self.i += 1;
                Ok(Tok::Open)
            }
            b'}' => {
                self.i += 1;
                Ok(Tok::Close)
            }
            b'"' => {
                self.i += 1;
                let mut out = Vec::new();
                while self.i < self.s.len() && self.s[self.i] != b'"' {
                    let c = self.s[self.i];
                    if c == b'\\' && self.i + 1 < self.s.len() {
                        self.i += 1;
                        out.push(match self.s[self.i] {
                            b'n' => b'\n',
                            b't' => b'\t',
                            b'r' => b'\r',
                            other => other,
                        });
                    } else {
                        out.push(c);
                    }
                    self.i += 1;
                }
                if self.i >= self.s.len() {
                    bail!("unterminated string in VDF");
                }
                self.i += 1;
                Ok(Tok::Str(String::from_utf8_lossy(&out).into_owned()))
            }
            _ => {
                let start = self.i;
                while self.i < self.s.len() && !self.s[self.i].is_ascii_whitespace() && !matches!(self.s[self.i], b'{' | b'}' | b'"') {
                    self.i += 1;
                }
                Ok(Tok::Str(String::from_utf8_lossy(&self.s[start..self.i]).into_owned()))
            }
        }
    }

    fn obj(&mut self, top: bool) -> Result<Obj> {
        let mut o = Obj::default();
        loop {
            match self.tok()? {
                Tok::Eof if top => return Ok(o),
                Tok::Eof => bail!("unexpected end of VDF"),
                Tok::Close if !top => return Ok(o),
                Tok::Close => bail!("unbalanced '}}' in VDF"),
                Tok::Open => bail!("unexpected '{{' in VDF"),
                Tok::Str(key) => match self.tok()? {
                    Tok::Str(v) => o.0.push((key, Value::Str(v))),
                    Tok::Open => {
                        let child = self.obj(false)?;
                        o.0.push((key, Value::Obj(child)));
                    }
                    _ => bail!("key '{key}' has no value in VDF"),
                },
            }
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub fn to_text(o: &Obj) -> String {
    let mut out = String::new();
    write_obj(o, 0, &mut out);
    out
}

fn write_obj(o: &Obj, depth: usize, out: &mut String) {
    let ind = "\t".repeat(depth);
    for (k, v) in &o.0 {
        match v {
            Value::Str(s) => out.push_str(&format!("{ind}\"{}\"\t\t\"{}\"\n", escape(k), escape(s))),
            Value::Obj(child) => {
                out.push_str(&format!("{ind}\"{}\"\n{ind}{{\n", escape(k)));
                write_obj(child, depth + 1, out);
                out.push_str(&format!("{ind}}}\n"));
            }
        }
    }
}

// ---------- binary (shortcuts.vdf) ----------

#[derive(Clone, Debug, PartialEq)]
pub enum BinValue {
    Map(Vec<(String, BinValue)>),
    Str(String),
    Int(u32),
    /// Other types (uint64, float, …) kept as raw bytes with their type tag so we can write them back.
    Raw(u8, Vec<u8>),
}

pub fn parse_binary(data: &[u8]) -> Result<Vec<(String, BinValue)>> {
    let mut i = 0usize;
    let map = read_map(data, &mut i).context("shortcuts.vdf is damaged")?;
    Ok(map)
}

fn read_cstr(d: &[u8], i: &mut usize) -> Result<String> {
    let start = *i;
    while *i < d.len() && d[*i] != 0 {
        *i += 1;
    }
    if *i >= d.len() {
        bail!("truncated string");
    }
    let s = String::from_utf8_lossy(&d[start..*i]).into_owned();
    *i += 1;
    Ok(s)
}

fn read_map(d: &[u8], i: &mut usize) -> Result<Vec<(String, BinValue)>> {
    let mut out = Vec::new();
    loop {
        if *i >= d.len() {
            // Tolerate a missing final terminator (seen in files written by some tools).
            return Ok(out);
        }
        let ty = d[*i];
        *i += 1;
        if ty == 0x08 {
            return Ok(out);
        }
        let key = read_cstr(d, i)?;
        let v = match ty {
            0x00 => BinValue::Map(read_map(d, i)?),
            0x01 => BinValue::Str(read_cstr(d, i)?),
            0x02 => {
                if *i + 4 > d.len() {
                    bail!("truncated int");
                }
                let v = u32::from_le_bytes(d[*i..*i + 4].try_into().unwrap());
                *i += 4;
                BinValue::Int(v)
            }
            0x03 | 0x04 | 0x06 => {
                if *i + 4 > d.len() {
                    bail!("truncated value");
                }
                let raw = d[*i..*i + 4].to_vec();
                *i += 4;
                BinValue::Raw(ty, raw)
            }
            0x07 | 0x0A => {
                if *i + 8 > d.len() {
                    bail!("truncated value");
                }
                let raw = d[*i..*i + 8].to_vec();
                *i += 8;
                BinValue::Raw(ty, raw)
            }
            other => bail!("unknown binary VDF type {other:#x}"),
        };
        out.push((key, v));
    }
}

pub fn to_binary(map: &[(String, BinValue)]) -> Vec<u8> {
    let mut out = Vec::new();
    write_map(map, &mut out);
    out
}

fn write_map(map: &[(String, BinValue)], out: &mut Vec<u8>) {
    for (k, v) in map {
        let (ty, body): (u8, Vec<u8>) = match v {
            BinValue::Map(_) => (0x00, Vec::new()),
            BinValue::Str(s) => (0x01, [s.as_bytes(), &[0]].concat()),
            BinValue::Int(n) => (0x02, n.to_le_bytes().to_vec()),
            BinValue::Raw(t, b) => (*t, b.clone()),
        };
        out.push(ty);
        out.extend_from_slice(k.as_bytes());
        out.push(0);
        if let BinValue::Map(m) = v {
            write_map(m, out);
        } else {
            out.extend_from_slice(&body);
        }
    }
    out.push(0x08);
}

/// CRC-32 (IEEE), used by Steam to derive non-Steam shortcut app ids.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip_and_lookup() {
        let src = r#""UserLocalConfigStore"
{
	"Software" { "Valve" { "Steam" { "apps" {
		"1245620" { "LaunchOptions"		"-skip \"x\"" "LastPlayed" "1" }
	} } } }
	// comment
	"flag" "1" [$WIN32]
}"#;
        let o = parse_text(src).unwrap();
        let apps = o.obj_path(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", "1245620"]).unwrap();
        assert_eq!(apps.str("launchoptions"), Some("-skip \"x\""));
        let again = parse_text(&to_text(&o)).unwrap();
        assert_eq!(o, again);
    }

    #[test]
    fn binary_roundtrip() {
        let map = vec![(
            "shortcuts".to_string(),
            BinValue::Map(vec![(
                "0".into(),
                BinValue::Map(vec![
                    ("appid".into(), BinValue::Int(0x8123_4567)),
                    ("AppName".into(), BinValue::Str("BonfireLink".into())),
                    ("LastPlayTime".into(), BinValue::Raw(0x07, vec![1, 2, 3, 4, 5, 6, 7, 8])),
                    ("tags".into(), BinValue::Map(vec![])),
                ]),
            )]),
        )];
        let bytes = to_binary(&map);
        assert_eq!(parse_binary(&bytes).unwrap(), map);
    }

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}

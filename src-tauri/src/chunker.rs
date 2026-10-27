use regex::Regex;
use std::sync::LazyLock;

static URLS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:https?://|www\.)\S+").expect("constant URL regex"));
static BULLETS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:[-+*•]+|\d+[.)])\s+").expect("constant bullet regex"));

pub fn sanitize(text: &str) -> String {
    let mut without_code = String::new();
    for (index, part) in text.split("```").enumerate() {
        if index % 2 == 0 {
            without_code.push_str(part);
            without_code.push(' ');
        }
    }
    let without_urls = URLS.replace_all(&without_code, "");
    let without_bullets = BULLETS.replace_all(&without_urls, "");
    let plain: String = without_bullets
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || ".,?!;:…¿¡'\"()-+%/".contains(*c))
        .collect();
    plain
        .replace("()", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Default)]
pub struct Chunker {
    pending: String,
    emitted: bool,
}
impl Chunker {
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.pending.push_str(text);
        let mut result = Vec::new();
        while let Some(end) = self.boundary(false) {
            let raw: String = self.pending.drain(..end).collect();
            let spoken = sanitize(&raw);
            if !spoken.is_empty() {
                self.emitted = true;
                result.push(spoken);
            }
        }
        result
    }
    pub fn finish(&mut self) -> Vec<String> {
        let mut result = Vec::new();
        while let Some(end) = self.boundary(true) {
            let raw: String = self.pending.drain(..end).collect();
            let spoken = sanitize(&raw);
            if !spoken.is_empty() {
                self.emitted = true;
                result.push(spoken);
            }
        }
        let spoken = sanitize(&std::mem::take(&mut self.pending));
        if !spoken.is_empty() {
            self.emitted = true;
            result.push(spoken);
        }
        result
    }
    fn boundary(&self, finished: bool) -> Option<usize> {
        let text = &self.pending;
        let mut in_code = false;
        let mut skip_until = 0;
        let mut last_space = None;
        for (chars, (index, c)) in text.char_indices().enumerate() {
            if index < skip_until {
                continue;
            }
            if text[index..].starts_with("```") {
                in_code = !in_code;
                skip_until = index + 3;
                continue;
            }
            if in_code {
                continue;
            }
            // Wait for enough bytes to distinguish a fence split across deltas.
            if c == '`' && text[index..].len() < 3 && !finished {
                return None;
            }
            if c.is_whitespace() {
                last_space = Some(index + c.len_utf8());
            }
            if chars >= 200 {
                if let Some(space) = last_space {
                    if space > 0 {
                        return Some(space);
                    }
                }
            }
            if !".?!…;\n,:".contains(c) {
                continue;
            }
            let end = index + c.len_utf8();
            let token_start = text[..index]
                .char_indices()
                .rev()
                .find(|(_, c)| c.is_whitespace())
                .map_or(0, |(p, c)| p + c.len_utf8());
            let token = &text[token_start..end];
            let normalized_token = token.to_ascii_lowercase();
            if normalized_token.starts_with("http") || normalized_token.starts_with("www.") {
                continue;
            }
            let next = text[end..].chars().next();
            if matches!(c, '.' | ',' | ':') {
                if next.is_none() && !finished {
                    return None;
                }
                let previous = text[..index].chars().next_back();
                if previous.is_some_and(|p| p.is_ascii_digit())
                    && next.is_some_and(|n| n.is_ascii_digit())
                {
                    continue;
                }
            }
            if c == '.' {
                let word = token
                    .trim_matches(|c: char| !c.is_alphabetic() && c != '.')
                    .trim_end_matches('.')
                    .to_lowercase();
                if [
                    "sr", "sra", "srta", "dr", "dra", "ud", "uds", "etc", "ej", "p", "p.ej",
                    "aprox", "núm", "nº", "av", "avda", "tel", "prof", "ing", "lic", "atte", "a",
                    "m", "ee", "uu",
                ]
                .contains(&word.as_str())
                {
                    continue;
                }
                if word.chars().count() == 1 && word.chars().all(char::is_alphabetic) {
                    continue;
                }
                if next.is_some_and(|n| n == '.' || n.is_alphanumeric()) {
                    continue;
                }
                if token
                    .trim_end_matches('.')
                    .chars()
                    .all(|c| c.is_ascii_digit())
                    && text[..token_start].trim().is_empty()
                {
                    continue;
                }
            }
            if (c == ',' || c == ':')
                && (self.emitted || sanitize(&text[..end]).split_whitespace().count() < 4)
            {
                continue;
            }
            return Some(end);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chunks(text: &str) -> Vec<String> {
        let mut c = Chunker::default();
        let mut result = c.push(text);
        result.extend(c.finish());
        result
    }
    macro_rules! case {
        ($name:ident,$text:expr,$expected:expr) => {
            #[test]
            fn $name() {
                assert_eq!(chunks($text), $expected);
            }
        };
    }
    case!(sentence, "Hola. Adiós.", vec!["Hola.", "Adiós."]);
    case!(
        unicode_space,
        "Hola\u{2003}amigo. ¿Todo\u{a0}bien?",
        vec!["Hola amigo.", "¿Todo bien?"]
    );
    case!(
        uppercase_url,
        "Puedes leer más en HTTPS://EXAMPLE.COM/test. Después hablamos.",
        vec!["Puedes leer más en Después hablamos."]
    );
    case!(
        question,
        "¿Cómo estás? Bien.",
        vec!["¿Cómo estás?", "Bien."]
    );
    case!(exclamation, "¡Claro! Vamos.", vec!["¡Claro!", "Vamos."]);
    case!(
        unicode_ellipsis,
        "Pues… seguimos",
        vec!["Pues…", "seguimos"]
    );
    case!(
        ascii_ellipsis,
        "Pues... seguimos",
        vec!["Pues...", "seguimos"]
    );
    case!(semicolon, "Uno; dos", vec!["Uno;", "dos"]);
    case!(newline, "Hola\nMundo", vec!["Hola", "Mundo"]);
    case!(
        first_comma,
        "Claro que puedo hacerlo, ahora mismo.",
        vec!["Claro que puedo hacerlo,", "ahora mismo."]
    );
    case!(short_comma, "Claro, lo hago.", vec!["Claro, lo hago."]);
    case!(
        first_colon,
        "Te lo explico así: funciona.",
        vec!["Te lo explico así:", "funciona."]
    );
    case!(
        subsequent_comma,
        "Hola. Esto tiene cuatro palabras, y sigue.",
        vec!["Hola.", "Esto tiene cuatro palabras, y sigue."]
    );
    case!(decimal, "Cuesta 3.14 pesos.", vec!["Cuesta 3.14 pesos."]);
    case!(
        decimal_comma,
        "Cuesta 3,14 pesos.",
        vec!["Cuesta 3,14 pesos."]
    );
    case!(time, "Son las 12:30 ahora.", vec!["Son las 12:30 ahora."]);
    case!(senor, "El Sr. López viene.", vec!["El Sr. López viene."]);
    case!(senora, "La Sra. López viene.", vec!["La Sra. López viene."]);
    case!(doctor, "El Dr. Pérez viene.", vec!["El Dr. Pérez viene."]);
    case!(
        doctora,
        "La Dra. Pérez viene.",
        vec!["La Dra. Pérez viene."]
    );
    case!(usted, "Ud. puede entrar.", vec!["Ud. puede entrar."]);
    case!(
        etcetera,
        "Pan, leche, etc. para hoy.",
        vec!["Pan, leche, etc. para hoy."]
    );
    case!(ejemplo, "Por ej. mañana.", vec!["Por ej. mañana."]);
    case!(initials, "J. Pérez llega.", vec!["J. Pérez llega."]);
    case!(
        url,
        "Visita https://ejemplo.com/ruta?q=1.2 y vuelve.",
        vec!["Visita y vuelve."]
    );
    case!(markdown, "**Claro** _amigo_ `sí`.", vec!["Claro amigo sí."]);
    case!(
        code_fence,
        "Antes. ```rust\nfn main() {}\n``` Después.",
        vec!["Antes.", "Después."]
    );
    case!(emoji, "Hola 👋🙂 amigo.", vec!["Hola amigo."]);
    case!(empty, "   ", Vec::<String>::new());
    case!(accents, "Mañana será útil.", vec!["Mañana será útil."]);
    case!(list, "- Hola\n- Adiós", vec!["Hola", "Adiós"]);
    case!(numbered_list, "1. Hola\n2. Adiós", vec!["Hola", "Adiós"]);
    #[test]
    fn fragmented_decimal() {
        let mut c = Chunker::default();
        assert!(c.push("Cuesta 3.").is_empty());
        assert_eq!(c.push("14 euros. Sí"), vec!["Cuesta 3.14 euros."]);
    }
    #[test]
    fn fragmented_abbreviation() {
        let mut c = Chunker::default();
        assert!(c.push("El Dr.").is_empty());
        assert_eq!(c.push(" Pérez viene. Ahora"), vec!["El Dr. Pérez viene."]);
    }
    #[test]
    fn max_length() {
        let input = "palabra ".repeat(80);
        let c = chunks(&input);
        assert!(c.len() >= 3);
        assert!(c.iter().all(|s| s.chars().count() <= 207));
        assert_eq!(c.join(" "), input.trim());
    }
    #[test]
    fn every_character_boundary() {
        let text = "El Dr. Pérez usa 3.14 euros. ¿Todo bien?";
        let mut c = Chunker::default();
        let mut result = Vec::new();
        for ch in text.chars() {
            result.extend(c.push(&ch.to_string()));
        }
        result.extend(c.finish());
        assert_eq!(result, chunks(text));
    }
    #[test]
    fn unfinished_code_is_silent() {
        assert_eq!(chunks("Hola ```rust\nfn f() {"), vec!["Hola"]);
    }
}

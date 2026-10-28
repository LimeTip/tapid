use super::invalid;
use std::io;

#[derive(Clone)]
pub(super) struct Pattern {
    components: Vec<String>,
    basename: bool,
    directory: bool,
    pub(super) negated: bool,
    pub(super) base: String,
}
impl Pattern {
    pub(super) fn parse(text: &str, ignore: bool, base: &str) -> io::Result<Self> {
        if !ignore && text.starts_with('!') {
            return Err(invalid("files negation is unsupported"));
        }
        let (negated, text) = if ignore && text.starts_with('!') {
            (true, &text[1..])
        } else {
            (false, text)
        };
        let anchored = text.starts_with('/');
        if anchored && !ignore {
            return Err(invalid("files patterns must be relative"));
        }
        let text = text.strip_prefix('/').unwrap_or(text);
        let text = text.strip_prefix("./").unwrap_or(text);
        let directory = text.ends_with('/');
        let text = text.strip_suffix('/').unwrap_or(text);
        let components: Vec<String> = text.split('/').map(str::to_owned).collect();
        if components.iter().any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.contains(['\\', '[', ']', '{', '}', ':'])
                || ["@(", "+(", "?(", "*(", "!("]
                    .iter()
                    .any(|syntax| component.contains(syntax))
                || component.chars().any(char::is_control)
                || (component.contains("**") && component != "**")
        }) {
            return Err(invalid(format!("unsupported packaging pattern: {text:?}")));
        }
        Ok(Self {
            basename: ignore && !anchored && components.len() == 1,
            components,
            directory,
            negated,
            base: base.to_owned(),
        })
    }
    pub(super) fn matches(&self, path: &str, steps: &mut u64) -> io::Result<bool> {
        let relative = if self.base.is_empty() {
            path
        } else {
            match path
                .strip_prefix(&self.base)
                .and_then(|path| path.strip_prefix('/'))
            {
                Some(path) => path,
                None => return Ok(false),
            }
        };
        let components: Vec<&str> = relative.split('/').collect();
        // Every prefix represents a containing directory; matching it includes descendants.
        for end in 1..=components.len() {
            if self.directory && end == components.len() {
                continue;
            }
            if self.basename {
                if wildcard(&self.components[0], components[end - 1], steps)? {
                    return Ok(true);
                }
            } else if paths_match(&self.components, &components[..end], steps)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
fn step(steps: &mut u64) -> io::Result<()> {
    *steps = steps
        .checked_sub(1)
        .ok_or_else(|| invalid("pattern matching work limit exceeded"))?;
    Ok(())
}
fn paths_match(pattern: &[String], path: &[&str], steps: &mut u64) -> io::Result<bool> {
    let mut previous = vec![false; path.len() + 1];
    previous[0] = true;
    for component in pattern {
        let mut next = vec![false; path.len() + 1];
        if component == "**" {
            next[0] = previous[0];
        }
        for index in 1..=path.len() {
            step(steps)?;
            next[index] = if component == "**" {
                previous[index] || next[index - 1]
            } else {
                previous[index - 1] && wildcard(component, path[index - 1], steps)?
            };
        }
        previous = next;
    }
    Ok(previous[path.len()])
}
fn wildcard(pattern: &str, value: &str, steps: &mut u64) -> io::Result<bool> {
    let value: Vec<char> = value.chars().collect();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for character in pattern.chars() {
        let mut next = vec![false; value.len() + 1];
        if character == '*' {
            next[0] = previous[0];
        }
        for index in 1..=value.len() {
            step(steps)?;
            next[index] = if character == '*' {
                previous[index] || next[index - 1]
            } else {
                previous[index - 1] && (character == '?' || character == value[index - 1])
            };
        }
        previous = next;
    }
    Ok(previous[value.len()])
}

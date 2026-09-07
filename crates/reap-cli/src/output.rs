//! Human-readable output: colour handling and the small table renderer the
//! report uses.

use std::io::IsTerminal;

#[derive(Debug, Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    /// Colour is on only for a terminal, and off whenever `--no-color`, the
    /// `NO_COLOR` convention or `--json` says so.
    pub fn detect(no_color: bool, json: bool) -> Self {
        let color = !no_color
            && !json
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal();
        Self { color }
    }

    #[cfg(test)]
    pub fn plain() -> Self {
        Self { color: false }
    }

    fn wrap(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    }

    pub fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub fn green(&self, s: &str) -> String {
        self.wrap("32", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.wrap("33", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.wrap("31", s)
    }
}

/// A left-aligned text table with a header rule.
///
/// Column widths are measured in characters rather than bytes so a path with
/// non-ASCII in it does not skew the layout.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    right_align: Vec<bool>,
}

impl Table {
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|h| (*h).to_owned()).collect(),
            rows: Vec::new(),
            right_align: vec![false; headers.len()],
        }
    }

    /// Marks a column as right-aligned. Used for byte counts, where a ragged
    /// right edge makes sizes hard to compare at a glance.
    pub fn right_align(mut self, col: usize) -> Self {
        if col < self.right_align.len() {
            self.right_align[col] = true;
        }
        self
    }

    pub fn push(&mut self, row: Vec<String>) {
        self.rows.push(row);
    }

    pub fn render(&self, style: &Style) -> String {
        let cols = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.chars().count()).collect();
        for row in &self.rows {
            for (i, cell) in row.iter().enumerate().take(cols) {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }

        let mut out = String::new();
        let header: Vec<String> = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| pad(h, widths[i], self.right_align[i]))
            .collect();
        out.push_str(&style.bold(header.join("  ").trim_end()));
        out.push('\n');
        out.push_str(
            &style.dim(
                &widths
                    .iter()
                    .map(|w| "-".repeat(*w))
                    .collect::<Vec<_>>()
                    .join("  "),
            ),
        );
        out.push('\n');
        for row in &self.rows {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .take(cols)
                .map(|(i, c)| pad(c, widths[i], self.right_align[i]))
                .collect();
            out.push_str(cells.join("  ").trim_end());
            out.push('\n');
        }
        out
    }
}

fn pad(s: &str, width: usize, right: bool) -> String {
    let len = s.chars().count();
    let fill = width.saturating_sub(len);
    if right {
        format!("{}{s}", " ".repeat(fill))
    } else {
        format!("{s}{}", " ".repeat(fill))
    }
}

/// Shortens a path for display by replacing the home prefix with `~`.
pub fn tilde(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    if let Some(h) = home {
        if let Ok(rest) = path.strip_prefix(h) {
            if rest.as_os_str().is_empty() {
                return "~".to_owned();
            }
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn table_aligns_and_right_aligns() {
        let mut t = Table::new(&["PATH", "SIZE"]).right_align(1);
        t.push(vec!["a".into(), "1 B".into()]);
        t.push(vec!["longer".into(), "12 GiB".into()]);
        let rendered = t.render(&Style::plain());
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "PATH      SIZE");
        assert_eq!(lines[2], "a          1 B");
        assert_eq!(lines[3], "longer  12 GiB");
    }

    #[test]
    fn tilde_shortens_only_inside_home() {
        let home = Path::new("/home/someone");
        assert_eq!(tilde(Path::new("/home/someone/code"), Some(home)), "~/code");
        assert_eq!(tilde(Path::new("/home/someone"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/opt/build"), Some(home)), "/opt/build");
    }
}

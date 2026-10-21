//! Finding sessions by typing, fzf-style. Each word has to match the session's
//! project, title or name (or `project/name`): its letters in order, not
//! necessarily together, so `atrw` finds "Auth rework". `'word` matches as
//! typed, `^word` at the start, `word$` at the end, and `!word` leaves out what
//! matches. Lowercase matches either case; a capital only itself.

use std::collections::HashMap;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};

use crate::model::Session;

/// How well a session matched, and which characters did, per field (as indices
/// of graphemes, in order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hit {
    pub score: u32,
    pub project: Vec<usize>,
    pub title: Vec<usize>,
    pub name: Vec<usize>,
}

/// The sessions matching `query`, by id; None if the query has nothing to match
/// (it's blank).
pub fn hits<'a>(
    query: &str,
    sessions: impl IntoIterator<Item = &'a Session>,
) -> Option<HashMap<String, Hit>> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() {
        return None;
    }
    let mut matcher = Matcher::default();
    Some(
        sessions
            .into_iter()
            .filter_map(|s| Some((s.id.clone(), hit(&pattern, &mut matcher, s)?)))
            .collect(),
    )
}

fn hit(pattern: &Pattern, matcher: &mut Matcher, s: &Session) -> Option<Hit> {
    let label = s.label();
    let fields = [
        s.project.as_str(),
        s.display_title(),
        s.name.as_str(),
        label.as_str(),
    ];
    let mut marks: [Vec<u32>; 3] = Default::default();
    let mut score = 0;
    let (mut buf, mut found) = (Vec::new(), Vec::new());
    for atom in &pattern.atoms {
        if atom.negative {
            // Left out if any field matches.
            for field in fields {
                atom.score(Utf32Str::new(field, &mut buf), matcher)?;
            }
            continue;
        }
        // The field it matches best.
        let mut best: Option<(u16, usize, Vec<u32>)> = None;
        for (i, field) in fields.iter().enumerate() {
            found.clear();
            if let Some(n) = atom.indices(Utf32Str::new(field, &mut buf), matcher, &mut found)
                && best.as_ref().is_none_or(|(b, ..)| n > *b)
            {
                best = Some((n, i, found.clone()));
            }
        }
        let (n, field, found) = best?;
        score += u32::from(n);
        if field < 3 {
            marks[field].extend(found);
        } else {
            // In `project/name`: the project's characters, the slash, the name's.
            let slash = s.project.chars().count() as u32;
            for i in found {
                match i.cmp(&slash) {
                    std::cmp::Ordering::Less => marks[0].push(i),
                    std::cmp::Ordering::Greater => marks[2].push(i - slash - 1),
                    std::cmp::Ordering::Equal => {}
                }
            }
        }
    }
    let [project, title, name] = marks.map(|mut m| {
        m.sort_unstable();
        m.dedup();
        m.into_iter().map(|i| i as usize).collect()
    });
    Some(Hit {
        score,
        project,
        title,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::session;

    fn with(id: &str, project: &str, name: &str, title: Option<&str>) -> Session {
        let mut s = session(id);
        s.project = project.into();
        s.name = name.into();
        s.title = title.map(Into::into);
        s
    }

    fn sessions() -> Vec<Session> {
        vec![
            with("1", "webapp", "parser", Some("Fix the date parser")),
            with("2", "api", "claude-1", Some("Auth rework")),
            with("3", "docs", "changelog", None),
        ]
    }

    fn ids(query: &str) -> Vec<String> {
        let sessions = sessions();
        let mut ids: Vec<String> = hits(query, &sessions)
            .unwrap_or_default()
            .into_keys()
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn letters_in_order_in_any_field() {
        assert_eq!(ids("atrw"), ["2"]);
        assert_eq!(ids("WEBAPP"), Vec::<String>::new());
        assert_eq!(ids("webapp"), ["1"]);
        assert_eq!(ids("chlog"), ["3"]);
        // Every word has to match, each in a field of its own.
        assert_eq!(ids("api auth"), ["2"]);
        assert_eq!(ids("api parser"), Vec::<String>::new());
        // As the CLI addresses sessions.
        assert_eq!(ids("api/cl"), ["2"]);
        assert_eq!(ids("!api"), ["1", "3"]);
        assert_eq!(ids("'pars"), ["1"]);
        assert!(hits("  ", &sessions()).is_none());
    }

    #[test]
    fn marks_the_matched_characters() {
        let sessions = sessions();
        let all = hits("auth rwk", &sessions).unwrap();
        assert_eq!(all["2"].title, [0, 1, 2, 3, 5, 7, 10]);
        let all = hits("api/cl", &sessions).unwrap();
        assert_eq!(
            (&all["2"].project[..], &all["2"].name[..]),
            (&[0, 1, 2][..], &[0, 1][..])
        );
    }
}

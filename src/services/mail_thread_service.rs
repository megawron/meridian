// SPDX-License-Identifier: GPL-3.0-or-later
//
// RFC-822 conversation/thread identity helpers.
//
// Camel has a native folder-threading API, but the first GTK4-rs mail model
// also has to handle local EML/maildir/mbox data and partially loaded Camel
// pages.  This small, backend-independent layer therefore derives stable
// conversation membership from the standard Message-ID headers.  It does not
// use the subject as a primary key when message identifiers are available.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct ThreadIdentity<'a> {
    pub message_id: Option<&'a str>,
    pub in_reply_to: &'a [String],
    pub references: &'a [String],
    pub subject: &'a str,
    pub date_ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMembership {
    pub thread_id: String,
    pub size: usize,
    pub position: usize,
}

/// Normalize one RFC-822 message identifier for matching.
pub fn normalize_message_id(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches(|character| matches!(character, '<' | '>' | '"' | '\''))
        .trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

/// Extract one or more message identifiers from `References` or
/// `In-Reply-To`.  RFC-compliant headers use angle brackets, while a few
/// providers emit whitespace-separated bare identifiers; both forms are
/// accepted here.
pub fn extract_message_ids(value: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut remainder = value;

    while let Some(start) = remainder.find('<') {
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        if let Some(identifier) = normalize_message_id(&after_start[..end]) {
            if !identifiers.contains(&identifier) {
                identifiers.push(identifier);
            }
        }
        remainder = &after_start[end + 1..];
    }

    if identifiers.is_empty() {
        for token in value.split(|character: char| character.is_whitespace() || character == ',') {
            if let Some(identifier) = normalize_message_id(token) {
                if !identifiers.contains(&identifier) {
                    identifiers.push(identifier);
                }
            }
        }
    }

    identifiers
}

/// Return the normalized subject and whether a reply/forward prefix was stripped.
pub fn normalize_subject_with_reply(subject: &str) -> (String, bool) {
    let mut subject = subject.trim();
    let mut had_prefix = false;
    loop {
        let lower = subject.to_ascii_lowercase();
        let stripped = if lower.starts_with("re:") {
            Some(&subject[3..])
        } else if lower.starts_with("re[") {
            subject.find(']').and_then(|idx| {
                let rest = &subject[idx + 1..];
                rest.strip_prefix(':').or(Some(rest))
            })
        } else if lower.starts_with("fwd:") {
            Some(&subject[4..])
        } else if lower.starts_with("fw:") {
            Some(&subject[3..])
        } else if lower.starts_with("aw:") {
            Some(&subject[3..])
        } else if lower.starts_with("wg:") {
            Some(&subject[3..])
        } else if lower.starts_with("antw:") {
            Some(&subject[5..])
        } else if lower.starts_with("antwort:") {
            Some(&subject[8..])
        } else if lower.starts_with("sv:") {
            Some(&subject[3..])
        } else if lower.starts_with("vs:") {
            Some(&subject[3..])
        } else {
            None
        };

        if let Some(rest) = stripped {
            had_prefix = true;
            subject = rest.trim_start_matches(|c: char| c == ']' || c == ':' || c.is_whitespace());
        } else {
            break;
        }
    }
    (subject.trim().to_ascii_lowercase(), had_prefix)
}

/// Return the subject used by the conservative subject-only fallback.
/// Repeated reply/forward prefixes are removed so a message without usable
/// message identifiers can still join its obvious local conversation.
pub fn normalized_subject(subject: &str) -> String {
    normalize_subject_with_reply(subject).0
}

/// Assign stable thread IDs and positions to a snapshot of messages.
///
/// Messages are connected when they share a Message-ID, `In-Reply-To`, or
/// `References` identifier. When explicit references are missing, messages
/// sharing a normalized subject with a reply/forward indicator or within a
/// reasonable time window are grouped into the conversation.
/// Inside each thread, members are sorted chronologically (oldest first) so
/// the conversation originator is at position 0 (Root).
pub fn group_messages(inputs: &[ThreadIdentity<'_>]) -> Vec<ThreadMembership> {
    let mut sets = DisjointSets::new(inputs.len());
    let mut identifier_owner = HashMap::<String, usize>::with_capacity(inputs.len());
    let mut identifiers = Vec::with_capacity(inputs.len());

    // Phase 1: Connect by RFC-822 message identifiers
    for (index, input) in inputs.iter().enumerate() {
        let mut message_identifiers = Vec::new();
        if let Some(message_id) = input.message_id.and_then(normalize_message_id) {
            message_identifiers.push(message_id);
        }
        for identifier in input
            .in_reply_to
            .iter()
            .chain(input.references.iter())
            .filter_map(|identifier| normalize_message_id(identifier))
        {
            if !message_identifiers.contains(&identifier) {
                message_identifiers.push(identifier);
            }
        }

        for identifier in &message_identifiers {
            if let Some(previous) = identifier_owner.insert(identifier.clone(), index) {
                sets.union(index, previous);
            }
        }
        identifiers.push(message_identifiers);
    }

    // Phase 2: Subject matching fallback.
    // Connect messages sharing the same normalized subject if at least one
    // has a reply/forward prefix or if they are within 90 days.
    let mut subject_groups = HashMap::<String, Vec<usize>>::new();
    for (index, input) in inputs.iter().enumerate() {
        let (subj, _) = normalize_subject_with_reply(input.subject);
        if !subj.is_empty()
            && subj != "ohne betreff"
            && subj != "(ohne betreff)"
            && subj != "no subject"
            && subj != "(no subject)"
            && subj != "kein betreff"
            && subj != "(kein betreff)"
        {
            subject_groups.entry(subj).or_default().push(index);
        }
    }

    for (_subj, member_indices) in subject_groups {
        if member_indices.len() <= 1 {
            continue;
        }
        let any_has_prefix = member_indices.iter().any(|&idx| {
            normalize_subject_with_reply(inputs[idx].subject).1
        });
        let first_idx = member_indices[0];
        let first_ts = inputs[first_idx].date_ts;
        for &other_idx in &member_indices[1..] {
            let other_ts = inputs[other_idx].date_ts;
            let within_window = first_ts == 0 || other_ts == 0 || (first_ts - other_ts).abs() < 90 * 86400;
            if any_has_prefix || within_window {
                sets.union(first_idx, other_idx);
            }
        }
    }

    let mut component_members = HashMap::<usize, Vec<usize>>::new();
    for index in 0..inputs.len() {
        component_members
            .entry(sets.find(index))
            .or_default()
            .push(index);
    }

    let mut memberships = vec![
        ThreadMembership {
            thread_id: String::new(),
            size: 1,
            position: 0,
        };
        inputs.len()
    ];

    for mut members in component_members.into_values() {
        // Sort chronologically: oldest message first!
        members.sort_by(|&a, &b| {
            let ta = inputs[a].date_ts;
            let tb = inputs[b].date_ts;
            if ta != 0 && tb != 0 && ta != tb {
                ta.cmp(&tb)
            } else {
                a.cmp(&b)
            }
        });

        let concrete_id = members.iter().find_map(|&index| {
            inputs[index]
                .message_id
                .and_then(normalize_message_id)
                .or_else(|| identifiers[index].first().cloned())
        });
        let thread_id = concrete_id.unwrap_or_else(|| {
            let subject = normalized_subject(inputs[members[0]].subject);
            format!("subject:{subject}")
        });

        let thread_id = if thread_id == "subject:" {
            format!("subject:(ohne Betreff):{}", members[0])
        } else {
            thread_id
        };

        for (position, index) in members.iter().enumerate() {
            memberships[*index] = ThreadMembership {
                thread_id: thread_id.clone(),
                size: members.len(),
                position,
            };
        }
    }

    memberships
}

#[derive(Debug)]
struct DisjointSets {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl DisjointSets {
    fn new(size: usize) -> Self {
        Self {
            parent: (0..size).collect(),
            rank: vec![0; size],
        }
    }

    fn find(&mut self, value: usize) -> usize {
        if self.parent[value] != value {
            let root = self.find(self.parent[value]);
            self.parent[value] = root;
        }
        self.parent[value]
    }

    fn union(&mut self, left: usize, right: usize) {
        let mut left = self.find(left);
        let mut right = self.find(right);
        if left == right {
            return;
        }
        if self.rank[left] < self.rank[right] {
            std::mem::swap(&mut left, &mut right);
        }
        self.parent[right] = left;
        if self.rank[left] == self.rank[right] {
            self.rank[left] += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_and_normalizes_angle_bracket_identifiers() {
        assert_eq!(
            extract_message_ids("<Root@Example.org> <Reply@example.org>"),
            vec!["root@example.org", "reply@example.org"]
        );
    }

    #[test]
    fn groups_transitive_reply_chain_by_root_message() {
        let root_reference = ["root@example.org".to_string()];
        let reply_reference = ["reply@example.org".to_string()];
        let inputs = vec![
            ThreadIdentity {
                message_id: Some("<root@example.org>"),
                in_reply_to: &[],
                references: &[],
                subject: "Project",
                date_ts: 100,
            },
            ThreadIdentity {
                message_id: Some("<reply@example.org>"),
                in_reply_to: &[],
                references: &root_reference,
                subject: "Re: Project",
                date_ts: 200,
            },
            ThreadIdentity {
                message_id: Some("<reply-2@example.org>"),
                in_reply_to: &reply_reference,
                references: &[],
                subject: "Re: Project",
                date_ts: 300,
            },
        ];

        let memberships = group_messages(&inputs);
        assert_eq!(memberships[0].thread_id, "root@example.org");
        assert_eq!(memberships[1].thread_id, "root@example.org");
        assert_eq!(memberships[2].thread_id, "root@example.org");
        assert_eq!(memberships[0].size, 3);
        assert_eq!(memberships[0].position, 0);
        assert_eq!(memberships[1].position, 1);
        assert_eq!(memberships[2].position, 2);
    }

    #[test]
    fn uses_subject_only_for_messages_without_identifiers() {
        let inputs = vec![
            ThreadIdentity {
                message_id: None,
                in_reply_to: &[],
                references: &[],
                subject: "Re: Status",
                date_ts: 200,
            },
            ThreadIdentity {
                message_id: None,
                in_reply_to: &[],
                references: &[],
                subject: "Status",
                date_ts: 100,
            },
        ];

        let memberships = group_messages(&inputs);
        assert_eq!(memberships[0].thread_id, "subject:status");
        assert_eq!(memberships[0].thread_id, memberships[1].thread_id);
        assert_eq!(memberships[0].size, 2);
        // Message 1 (Status at ts 100) should be position 0, Message 0 (Re: Status at ts 200) should be position 1
        assert_eq!(memberships[1].position, 0);
        assert_eq!(memberships[0].position, 1);
    }

    #[test]
    fn groups_by_subject_with_independent_ids_and_orders_chronologically() {
        let inputs = vec![
            // Günter arrived at ts 2000 (newer) with Re: Homepage
            ThreadIdentity {
                message_id: Some("<guenter@example.org>"),
                in_reply_to: &[],
                references: &[],
                subject: "Re: Homepage",
                date_ts: 2000,
            },
            // Kinga arrived at ts 1000 (older) with Homepage
            ThreadIdentity {
                message_id: Some("<kinga@example.org>"),
                in_reply_to: &[],
                references: &[],
                subject: "Homepage ",
                date_ts: 1000,
            },
        ];

        let memberships = group_messages(&inputs);
        assert_eq!(memberships[0].thread_id, memberships[1].thread_id);
        assert_eq!(memberships[0].size, 2);
        assert_eq!(memberships[1].size, 2);
        // Kinga (older) is root (position 0), Günter (newer) is reply (position 1)
        assert_eq!(memberships[1].position, 0);
        assert_eq!(memberships[0].position, 1);
    }
}

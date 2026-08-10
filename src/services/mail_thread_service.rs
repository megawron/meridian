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

/// Return the subject used by the conservative subject-only fallback.
/// Repeated reply/forward prefixes are removed so a message without usable
/// message identifiers can still join its obvious local conversation.
pub fn normalized_subject(subject: &str) -> String {
    let mut subject = subject.trim().to_ascii_lowercase();
    loop {
        let Some(rest) = subject
            .strip_prefix("re:")
            .or_else(|| subject.strip_prefix("re["))
            .or_else(|| subject.strip_prefix("fwd:"))
            .or_else(|| subject.strip_prefix("fw:"))
            .or_else(|| subject.strip_prefix("aw:"))
        else {
            break;
        };
        subject = rest
            .trim_start_matches(|character: char| {
                character == ']' || character == ':' || character.is_whitespace()
            })
            .to_string();
    }
    subject
}

/// Assign stable thread IDs and positions to a snapshot of messages.
///
/// Messages are connected when they share a Message-ID, `In-Reply-To`, or
/// `References` identifier.  Components prefer the first concrete message
/// identifier as their public thread ID, which keeps a partially loaded
/// conversation stable when more messages arrive later.  Only messages with
/// no usable identifiers use the normalized subject fallback.
pub fn group_messages(inputs: &[ThreadIdentity<'_>]) -> Vec<ThreadMembership> {
    let mut sets = DisjointSets::new(inputs.len());
    let mut identifier_owner = HashMap::<String, usize>::new();
    let mut subject_owner = HashMap::<String, usize>::new();
    let mut identifiers = Vec::with_capacity(inputs.len());

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
        if message_identifiers.is_empty() {
            let subject = normalized_subject(input.subject);
            if let Some(previous) = subject_owner.insert(subject, index) {
                sets.union(index, previous);
            }
        }
        identifiers.push(message_identifiers);
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

    for members in component_members.into_values() {
        let concrete_id = members.iter().find_map(|index| {
            inputs[*index]
                .message_id
                .and_then(normalize_message_id)
                .or_else(|| identifiers[*index].first().cloned())
        });
        let thread_id = concrete_id.unwrap_or_else(|| {
            let subject = normalized_subject(inputs[members[0]].subject);
            format!("subject:{subject}")
        });

        // A component with no identifiers is only possible for a subject
        // fallback.  Keep distinct empty subjects separate instead of
        // collapsing every unidentifiable mail into one conversation.
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
            },
            ThreadIdentity {
                message_id: Some("<reply@example.org>"),
                in_reply_to: &[],
                references: &root_reference,
                subject: "Re: Project",
            },
            ThreadIdentity {
                message_id: Some("<reply-2@example.org>"),
                in_reply_to: &reply_reference,
                references: &[],
                subject: "Re: Project",
            },
        ];

        let memberships = group_messages(&inputs);
        assert_eq!(memberships[0].thread_id, "root@example.org");
        assert_eq!(memberships[1].thread_id, "root@example.org");
        assert_eq!(memberships[2].thread_id, "root@example.org");
        assert_eq!(memberships[0].size, 3);
    }

    #[test]
    fn uses_subject_only_for_messages_without_identifiers() {
        let inputs = vec![
            ThreadIdentity {
                message_id: None,
                in_reply_to: &[],
                references: &[],
                subject: "Re: Status",
            },
            ThreadIdentity {
                message_id: None,
                in_reply_to: &[],
                references: &[],
                subject: "Status",
            },
        ];

        let memberships = group_messages(&inputs);
        assert_eq!(memberships[0].thread_id, "subject:status");
        assert_eq!(memberships[0].thread_id, memberships[1].thread_id);
        assert_eq!(memberships[0].size, 2);
    }
}

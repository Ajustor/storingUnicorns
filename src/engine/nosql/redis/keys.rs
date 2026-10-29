//! The key tree of the explorer: keys grouped by their prefixes.

use std::cmp::Ordering;

use super::{display_bytes, KeyInfo, KeyNode};

/// Keys grouped by their `sep`-separated prefixes: groups then keys,
/// sorted ignoring case. A group's label ends with `sep` (`user:`), a key's
/// label is its last segment (the whole key when that segment is empty).
pub fn key_tree(keys: &[KeyInfo], sep: char) -> Vec<KeyNode> {
    let names: Vec<String> = keys.iter().map(|k| display_bytes(&k.key)).collect();
    let items: Vec<(usize, &str)> = names.iter().map(String::as_str).enumerate().collect();
    build(&names, items, "", sep)
}

/// Nodes of the keys `items` (index, rest of the name after `prefix`).
fn build(names: &[String], items: Vec<(usize, &str)>, prefix: &str, sep: char) -> Vec<KeyNode> {
    // Groups in order of first appearance, then sorted.
    let mut groups: Vec<(String, Vec<(usize, &str)>)> = Vec::new();
    let mut leaves = Vec::new();
    for (index, rest) in items {
        match rest.find(sep) {
            Some(at) => {
                let label = &rest[..at + sep.len_utf8()];
                let remainder = &rest[at + sep.len_utf8()..];
                match groups.iter_mut().find(|(l, _)| l == label) {
                    Some((_, members)) => members.push((index, remainder)),
                    None => groups.push((label.to_string(), vec![(index, remainder)])),
                }
            }
            None => leaves.push(KeyNode {
                label: if rest.is_empty() {
                    names[index].clone()
                } else {
                    rest.to_string()
                },
                prefix: names[index].clone(),
                children: Vec::new(),
                key: Some(index),
                count: 1,
            }),
        }
    }
    let mut nodes: Vec<KeyNode> = groups
        .into_iter()
        .map(|(label, members)| {
            let full = format!("{prefix}{label}");
            let children = build(names, members, &full, sep);
            KeyNode {
                count: children.iter().map(|c| c.count).sum(),
                label,
                prefix: full,
                children,
                key: None,
            }
        })
        .collect();
    nodes.sort_by(by_label);
    leaves.sort_by(by_label);
    nodes.extend(leaves);
    nodes
}

fn by_label(a: &KeyNode, b: &KeyNode) -> Ordering {
    a.label
        .to_lowercase()
        .cmp(&b.label.to_lowercase())
        .then_with(|| a.label.cmp(&b.label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::nosql::redis::RedisType;

    fn keys(names: &[&str]) -> Vec<KeyInfo> {
        names
            .iter()
            .map(|n| KeyInfo {
                key: n.as_bytes().to_vec(),
                kind: RedisType::String,
                ttl: None,
                size: None,
            })
            .collect()
    }

    fn labels(nodes: &[KeyNode]) -> Vec<&str> {
        nodes.iter().map(|n| n.label.as_str()).collect()
    }

    #[test]
    fn nested_prefixes() {
        let k = keys(&[
            "user:1:name",
            "user:1:mail",
            "user:2:name",
            "session:x",
            "config",
        ]);
        let tree = key_tree(&k, ':');
        assert_eq!(labels(&tree), ["session:", "user:", "config"]);
        let user = &tree[1];
        assert_eq!(user.prefix, "user:");
        assert_eq!(user.count, 3);
        assert_eq!(user.key, None);
        assert_eq!(labels(&user.children), ["1:", "2:"]);
        let one = &user.children[0];
        assert_eq!(one.prefix, "user:1:");
        assert_eq!(one.count, 2);
        assert_eq!(labels(&one.children), ["mail", "name"]);
        assert_eq!(one.children[0].key, Some(1));
        assert_eq!(one.children[0].prefix, "user:1:mail");
        assert_eq!(tree[2].key, Some(4));
        assert_eq!(tree[2].count, 1);
    }

    #[test]
    fn a_key_can_equal_a_prefix() {
        let k = keys(&["user", "user:1"]);
        let tree = key_tree(&k, ':');
        assert_eq!(labels(&tree), ["user:", "user"]);
        assert_eq!(tree[0].children[0].prefix, "user:1");
        assert_eq!(tree[1].key, Some(0));
    }

    #[test]
    fn sorted_ignoring_case() {
        let k = keys(&["b", "A", "a", "C:x", "b:y"]);
        let tree = key_tree(&k, ':');
        assert_eq!(labels(&tree), ["b:", "C:", "A", "a", "b"]);
    }

    #[test]
    fn empty_segments_and_other_separators() {
        let k = keys(&["a::b", "a:"]);
        let tree = key_tree(&k, ':');
        assert_eq!(labels(&tree), ["a:"]);
        let a = &tree[0];
        assert_eq!(a.count, 2);
        assert_eq!(labels(&a.children), [":", "a:"]);
        assert_eq!(a.children[0].prefix, "a::");
        assert_eq!(key_tree(&keys(&["x/y"]), '/')[0].label, "x/");
        assert!(key_tree(&[], ':').is_empty());
    }
}

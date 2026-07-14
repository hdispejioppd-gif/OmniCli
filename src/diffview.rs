//! Interactive diff review: build, navigate, accept/reject hunks, apply.

use std::path::PathBuf;

use similar::TextDiff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkState {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
    pub state: HunkState,
}

#[derive(Debug, Clone)]
pub struct DiffView {
    pub path: PathBuf,
    pub hunks: Vec<Hunk>,
    pub cursor: usize,
}

impl DiffView {
    pub fn accept_current(&mut self) {
        if let Some(h) = self.hunks.get_mut(self.cursor) {
            h.state = HunkState::Accepted;
        }
        self.advance();
    }
    pub fn reject_current(&mut self) {
        if let Some(h) = self.hunks.get_mut(self.cursor) {
            h.state = HunkState::Rejected;
        }
        self.advance();
    }
    fn advance(&mut self) {
        for i in (self.cursor + 1)..self.hunks.len() {
            if self.hunks[i].state == HunkState::Pending {
                self.cursor = i;
                return;
            }
        }
        for i in 0..self.hunks.len() {
            if self.hunks[i].state == HunkState::Pending {
                self.cursor = i;
                return;
            }
        }
    }
    pub fn accept_all(&mut self) {
        for h in &mut self.hunks {
            if h.state == HunkState::Pending {
                h.state = HunkState::Accepted;
            }
        }
    }
    pub fn next(&mut self) {
        self.advance();
    }
    pub fn prev(&mut self) {
        for i in (0..self.cursor).rev() {
            if self.hunks[i].state == HunkState::Pending {
                self.cursor = i;
                return;
            }
        }
        for i in (0..self.hunks.len()).rev() {
            if self.hunks[i].state == HunkState::Pending {
                self.cursor = i;
                return;
            }
        }
    }
    pub fn all_resolved(&self) -> bool {
        !self.hunks.iter().any(|h| h.state == HunkState::Pending)
    }

    pub fn apply(&self, old: &str) -> String {
        let old_lines: Vec<&str> = old.lines().collect();
        let n = old_lines.len();
        let mut removed = vec![false; n];
        let mut content: Vec<String> = old_lines.iter().map(|s| s.to_string()).collect();
        let mut insertions: Vec<(usize, String)> = Vec::new();

        for hunk in &self.hunks {
            if hunk.state != HunkState::Accepted {
                continue;
            }
            let mut pos = 0usize;
            for line in &hunk.lines {
                match line.kind {
                    DiffKind::Removed => {
                        if let Some(no) = line.old_no
                            && no > 0
                            && no <= n
                        {
                            removed[no - 1] = true;
                        }
                        pos = pos.max(line.old_no.unwrap_or(0));
                    }
                    DiffKind::Added => {
                        insertions.push((pos, line.content.clone()));
                    }
                    DiffKind::Context => {
                        if let Some(no) = line.old_no {
                            pos = no;
                        }
                    }
                }
            }
        }
        insertions.sort_by_key(|(p, _)| *p);
        let mut result = Vec::new();
        let mut ins_idx = 0;
        for i in 0..n {
            while ins_idx < insertions.len() && insertions[ins_idx].0 < i + 1 {
                result.push(std::mem::take(&mut insertions[ins_idx].1));
                ins_idx += 1;
            }
            if !removed[i] {
                result.push(std::mem::take(&mut content[i]));
            }
            while ins_idx < insertions.len() && insertions[ins_idx].0 == i + 1 {
                result.push(std::mem::take(&mut insertions[ins_idx].1));
                ins_idx += 1;
            }
        }
        while ins_idx < insertions.len() {
            result.push(std::mem::take(&mut insertions[ins_idx].1));
            ins_idx += 1;
        }
        result.join("\n")
    }
}

pub fn build(path: PathBuf, old: &str, new: &str) -> DiffView {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();

    for group in diff.grouped_ops(0) {
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    similar::ChangeTag::Equal => DiffKind::Context,
                    similar::ChangeTag::Delete => DiffKind::Removed,
                    similar::ChangeTag::Insert => DiffKind::Added,
                };
                let old_no = change.old_index().map(|i| i + 1);
                let new_no = change.new_index().map(|i| i + 1);
                lines.push(DiffLine {
                    kind,
                    old_no,
                    new_no,
                    content: change.value().trim_end_matches('\n').to_string(),
                });
            }
        }
        if !lines.is_empty() {
            let first_old = lines.iter().find_map(|l| l.old_no).unwrap_or(0);
            let old_cnt = lines.iter().filter(|l| l.kind != DiffKind::Added).count();
            let new_first = lines.iter().find_map(|l| l.new_no).unwrap_or(0);
            let new_cnt = lines.iter().filter(|l| l.kind != DiffKind::Removed).count();
            let header = format!(
                "@@ -{},{} +{},{} @@",
                first_old, old_cnt, new_first, new_cnt
            );
            hunks.push(Hunk {
                header,
                lines,
                state: HunkState::Pending,
            });
        }
    }
    DiffView {
        path,
        hunks,
        cursor: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_produces_hunks() {
        let v = build(PathBuf::from("t.txt"), "a\nb\nc\n", "A\nb\nC\n");
        assert!(!v.hunks.is_empty());
    }

    #[test]
    fn accept_all_equals_new() {
        let old = "a\nb\nc";
        let new = "A\nb\nC";
        let mut v = build(PathBuf::from("f"), old, new);
        v.accept_all();
        assert_eq!(v.apply(old), new, "accept_all == new");
    }

    #[test]
    fn reject_all_equals_old() {
        let old = "a\nb\nc";
        let new = "X\nb\nY";
        let mut v = build(PathBuf::from("f"), old, new);
        for h in &mut v.hunks {
            h.state = HunkState::Rejected;
        }
        assert_eq!(v.apply(old), old, "reject_all == old");
    }

    #[test]
    fn accept_partial() {
        let old = "a\nb\nc";
        let new = "A\nb\nC";
        let mut v = build(PathBuf::from("f"), old, new);
        assert!(
            v.hunks.len() >= 2,
            "2 separate changes, got {}",
            v.hunks.len()
        );
        v.hunks[0].state = HunkState::Accepted;
        let r = v.apply(old);
        assert!(r.contains('A'), "first accepted: {}", r);
        assert!(r.contains('c'), "second rejected: {}", r);
    }
}

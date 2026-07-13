use std::collections::HashSet;

/// Line-set union of two JSONL byte buffers: remote lines first (their
/// order), then local-only lines (their order). Exact byte lines are the
/// units. Trailing partial (newline-less) lines are preserved at the end.
pub fn union_jsonl(local: &[u8], remote: &[u8]) -> Vec<u8> {
    fn split(data: &[u8]) -> (Vec<&[u8]>, Option<&[u8]>) {
        let mut complete = Vec::new();
        let mut partial = None;
        for line in data.split_inclusive(|&b| b == b'\n') {
            if line.ends_with(b"\n") {
                complete.push(line);
            } else {
                partial = Some(line);
            }
        }
        (complete, partial)
    }
    let (remote_lines, remote_partial) = split(remote);
    let (local_lines, local_partial) = split(local);

    let mut seen: HashSet<&[u8]> = HashSet::new();
    let mut out = Vec::with_capacity(local.len() + remote.len());
    for line in remote_lines.into_iter().chain(local_lines) {
        if seen.insert(line) {
            out.extend_from_slice(line);
        }
    }
    // Partial tails: keep both, remote first; separate distinct tails so two
    // different truncated lines never glue into one.
    let mut tail_written = false;
    for partial in [remote_partial, local_partial].into_iter().flatten() {
        if tail_written {
            if out.ends_with(partial) {
                continue;
            }
            out.push(b'\n');
        }
        out.extend_from_slice(partial);
        tail_written = true;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_no_duplicates_remote_first() {
        let remote = b"{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n";
        let local = b"{\"a\":1}\n{\"d\":4}\n{\"b\":2}\n";
        let merged = union_jsonl(local, remote);
        assert_eq!(
            String::from_utf8(merged).unwrap(),
            "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n{\"d\":4}\n"
        );
    }

    #[test]
    fn union_is_idempotent() {
        let a = b"x\ny\n";
        let once = union_jsonl(a, a);
        assert_eq!(once, a.to_vec());
        let twice = union_jsonl(&once, a);
        assert_eq!(twice, a.to_vec());
    }

    #[test]
    fn trailing_partial_lines_preserved_at_end() {
        let remote = b"{\"a\":1}\n";
        let local = b"{\"b\":2}\n{\"partial";
        let merged = union_jsonl(local, remote);
        assert_eq!(String::from_utf8(merged).unwrap(), "{\"a\":1}\n{\"b\":2}\n{\"partial");
    }
}

const CONFLICT_START: &str = "<<<<<<< remote (read only)";
const CONFLICT_MIDDLE: &str = "=======";
const CONFLICT_END: &str = ">>>>>>> local";

pub(crate) fn push_conflict_str(str: &mut String, remote: &str, local: &str) {
    str.push_str(CONFLICT_START);
    str.push('\n');
    str.push_str(remote);
    str.push_str(CONFLICT_MIDDLE);
    str.push('\n');
    str.push_str(local);
    str.push_str(CONFLICT_END);
    str.push('\n');
}

pub(crate) fn read_conflict_local(str: &str) -> String {
    let mut kept = String::new();
    let mut lines = str.lines();
    while let Some(line) = lines.next() {
        if line == CONFLICT_START {
            for line in lines.by_ref() {
                if line == CONFLICT_MIDDLE {
                    break;
                }
            }
            for line in lines.by_ref() {
                if line == CONFLICT_END {
                    break;
                }
                kept.push_str(line);
                kept.push('\n');
            }
        } else {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

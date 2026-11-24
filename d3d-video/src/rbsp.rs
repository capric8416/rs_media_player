use std::borrow::Cow;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Rbsp<'data> {
    pub(crate) data: Cow<'data, [u8]>,
    pub(crate) consumed: usize,
    pub(crate) skipped_positions: Vec<usize>,
}

pub(crate) fn extract_rbsp(input: &[u8]) -> Rbsp<'_> {
    let first_special = input
        .windows(3)
        .position(|bytes| bytes[0] == 0 && bytes[1] == 0 && (bytes[2] == 1 || bytes[2] == 3));
    let Some(first_special) = first_special else {
        return Rbsp {
            data: Cow::Borrowed(input),
            consumed: input.len(),
            skipped_positions: Vec::new(),
        };
    };
    if input[first_special + 2] == 1 {
        return Rbsp {
            data: Cow::Borrowed(&input[..first_special]),
            consumed: first_special,
            skipped_positions: Vec::new(),
        };
    }

    let mut output = Vec::with_capacity(input.len());
    output.extend_from_slice(&input[..first_special]);
    let mut skipped_positions = Vec::new();
    let mut source = first_special;
    while source + 2 < input.len() {
        if input[source] == 0 && input[source + 1] == 0 {
            if input[source + 2] == 3 {
                output.extend_from_slice(&[0, 0]);
                skipped_positions.push(output.len() - 1);
                source += 3;
                continue;
            }
            if input[source + 2] == 1 {
                return Rbsp {
                    data: Cow::Owned(output),
                    consumed: source,
                    skipped_positions,
                };
            }
        }
        output.push(input[source]);
        source += 1;
    }
    output.extend_from_slice(&input[source..]);
    Rbsp {
        data: Cow::Owned(output),
        consumed: input.len(),
        skipped_positions,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::extract_rbsp;

    #[test]
    fn borrows_when_no_escape_is_present() {
        let input = [0x67, 0x42, 0x00, 0x1f];
        let rbsp = extract_rbsp(&input);
        assert!(matches!(rbsp.data, Cow::Borrowed(_)));
        assert_eq!(rbsp.data.as_ref(), input);
        assert_eq!(rbsp.consumed, input.len());
    }

    #[test]
    fn removes_emulation_prevention_and_records_positions() {
        let input = [0x67, 0, 0, 3, 1, 0, 0, 3, 0x7f];
        let rbsp = extract_rbsp(&input);
        assert!(matches!(rbsp.data, Cow::Owned(_)));
        assert_eq!(rbsp.data.as_ref(), [0x67, 0, 0, 1, 0, 0, 0x7f]);
        assert_eq!(rbsp.skipped_positions, [2, 5]);
    }

    #[test]
    fn stops_before_a_following_start_code() {
        let input = [0x65, 0xaa, 0, 0, 1, 0x67];
        let rbsp = extract_rbsp(&input);
        assert_eq!(rbsp.data.as_ref(), [0x65, 0xaa]);
        assert_eq!(rbsp.consumed, 2);
    }
}

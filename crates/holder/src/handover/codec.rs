//! The record's byte form: LEB128 varints, length-prefixed bytes, and a
//! reader that checks every length against a bound before it allocates.

use super::RecordError;

/// Builds a record.
#[derive(Debug, Default)]
pub struct Writer {
    out: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.out
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    pub fn u8(&mut self, value: u8) {
        self.out.push(value);
    }

    pub fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    pub fn uvar(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.out.push((value as u8) | 0x80);
            value >>= 7;
        }
        self.out.push(value as u8);
    }

    pub fn usize(&mut self, value: usize) {
        self.uvar(value as u64);
    }

    /// A signed value, zigzag coded.
    pub fn ivar(&mut self, value: i64) {
        self.uvar(((value << 1) ^ (value >> 63)) as u64);
    }

    pub fn bytes(&mut self, bytes: &[u8]) {
        self.usize(bytes.len());
        self.raw(bytes);
    }

    pub fn str(&mut self, text: &str) {
        self.bytes(text.as_bytes());
    }

    pub fn char(&mut self, c: char) {
        self.uvar(u64::from(u32::from(c)));
    }

    pub fn option<T>(&mut self, value: Option<T>, put: impl FnOnce(&mut Self, T)) {
        match value {
            None => self.u8(0),
            Some(value) => {
                self.u8(1);
                put(self, value);
            }
        }
    }
}

/// Reads a record.
#[derive(Debug)]
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// Whether every byte has been read.
    pub fn done(&self) -> bool {
        self.at == self.bytes.len()
    }

    pub fn raw(&mut self, len: usize) -> Result<&'a [u8], RecordError> {
        let end = self
            .at
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(RecordError::Short)?;
        let raw = &self.bytes[self.at..end];
        self.at = end;
        Ok(raw)
    }

    pub fn u8(&mut self) -> Result<u8, RecordError> {
        Ok(self.raw(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, RecordError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(RecordError::Bad("a flag")),
        }
    }

    pub fn uvar(&mut self) -> Result<u64, RecordError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.u8()?;
            let bits = u64::from(byte & 0x7f);
            if shift == 63 && bits > 1 {
                return Err(RecordError::Bad("a number"));
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(RecordError::Bad("a number"))
    }

    /// A count or length of at most `max`.
    pub fn len(&mut self, max: usize, what: &'static str) -> Result<usize, RecordError> {
        let value = self.uvar()?;
        usize::try_from(value)
            .ok()
            .filter(|&len| len <= max)
            .ok_or(RecordError::Limit(what))
    }

    pub fn ivar(&mut self) -> Result<i64, RecordError> {
        let value = self.uvar()?;
        Ok(((value >> 1) as i64) ^ -((value & 1) as i64))
    }

    pub fn bytes(&mut self, max: usize, what: &'static str) -> Result<Vec<u8>, RecordError> {
        let len = self.len(max, what)?;
        Ok(self.raw(len)?.to_vec())
    }

    pub fn str(&mut self, max: usize, what: &'static str) -> Result<String, RecordError> {
        String::from_utf8(self.bytes(max, what)?).map_err(|_| RecordError::Bad(what))
    }

    pub fn char(&mut self) -> Result<char, RecordError> {
        u32::try_from(self.uvar()?)
            .ok()
            .and_then(char::from_u32)
            .ok_or(RecordError::Bad("a character"))
    }

    pub fn option<T>(
        &mut self,
        get: impl FnOnce(&mut Self) -> Result<T, RecordError>,
    ) -> Result<Option<T>, RecordError> {
        match self.u8()? {
            0 => Ok(None),
            1 => get(self).map(Some),
            _ => Err(RecordError::Bad("an option")),
        }
    }
}

/// FNV-1a, 64 bits: the record's and each pane's checksum.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn numbers_and_strings_round_trip(u in any::<u64>(), i in any::<i64>(), text in ".*") {
            let mut w = Writer::new();
            w.uvar(u);
            w.ivar(i);
            w.str(&text);
            let bytes = w.into_bytes();
            let mut r = Reader::new(&bytes);
            prop_assert_eq!(r.uvar().unwrap(), u);
            prop_assert_eq!(r.ivar().unwrap(), i);
            prop_assert_eq!(r.str(usize::MAX, "text").unwrap(), text);
            prop_assert!(r.done());
        }
    }

    #[test]
    fn a_short_or_overlong_number_is_refused() {
        assert!(matches!(
            Reader::new(&[0x80]).uvar(),
            Err(RecordError::Short)
        ));
        let mut long = vec![0xff; 9];
        long.push(0x02);
        assert!(matches!(
            Reader::new(&long).uvar(),
            Err(RecordError::Bad(_))
        ));
        let mut w = Writer::new();
        w.usize(10);
        let bytes = w.into_bytes();
        assert!(matches!(
            Reader::new(&bytes).len(9, "rows"),
            Err(RecordError::Limit("rows"))
        ));
    }
}

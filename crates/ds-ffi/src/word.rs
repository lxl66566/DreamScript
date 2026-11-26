//! The enumerated per-register-class transmute, shared by every
//! marshaling face: the call site's function-pointer type must match
//! the staged words' register classes exactly, so the placements are
//! enumerated up front — every GP/FP placement of at most
//! [`crate::MAX_PARAMS`] words spelled as one monomorphized call site,
//! chosen by the word count and the placement mask. The catch-all arm
//! serves the all-FP spelling for any wider shape a caller stages,
//! which is why every face checks the word cap before dispatching —
//! that check is the one backstop between a wider signature and a
//! mis-called symbol.

/// One marshaled argument word: the register class with its bit
/// pattern. A word carries a pointer or a length as an i64 bit-cast —
/// the enumerated targets rule pointers, integers, and buffer lengths
/// the same INTEGER register class at the same width, so the class
/// pairing is the ABI's own contract.
#[derive(Clone, Copy)]
pub(crate) enum ArgWord {
    /// The general-purpose register word.
    Gp(i64),
    /// The floating register word.
    Fp(f64),
}

/// The marshaled words' cursor: the call site reads one word per C
/// parameter position in order, converting through the class the
/// monomorphized function-pointer type demands.
pub(crate) struct Cursor<'a> {
    pub(crate) words: &'a [ArgWord],
    pub(crate) at: usize,
}

/// The general-purpose word's conversions: the return types the
/// enumerated call sites serve.
pub(crate) trait FromGp {
    /// The conversion off the GP word.
    fn from_gp(word: i64) -> Self;
}

/// The floating word's conversions.
pub(crate) trait FromFp {
    /// The conversion off the FP word.
    fn from_fp(word: f64) -> Self;
}

impl FromGp for i64 {
    fn from_gp(word: i64) -> Self {
        word
    }
}

impl FromGp for u8 {
    // the C-ABI contract: a u8 parameter reads the GP word's low byte
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn from_gp(word: i64) -> Self {
        word as u8
    }
}

impl FromFp for f64 {
    fn from_fp(word: f64) -> Self {
        word
    }
}

impl Cursor<'_> {
    /// Takes the next word as a general-purpose conversion.
    pub(crate) fn gp<R: FromGp>(&mut self) -> R {
        let word = self.words[self.at];
        self.at += 1;
        R::from_gp(match word {
            ArgWord::Gp(value) => value,
            // unreachable against a staged mask the caller checked; the
            // class pairing is the transmute's own contract
            ArgWord::Fp(_) => 0,
        })
    }

    /// Takes the next word as a floating conversion.
    pub(crate) fn fp<R: FromFp>(&mut self) -> R {
        let word = self.words[self.at];
        self.at += 1;
        R::from_fp(match word {
            ArgWord::Fp(value) => value,
            ArgWord::Gp(_) => 0.0,
        })
    }
}

/// The per-placement call: the macro takes the staged words, the
/// placement mask (bit i set when word i is FP), and the return type,
/// transmutes the symbol's address into the matching `unsafe extern "C"`
/// function-pointer type, and calls it. Each arm is a monomorphized call
/// site; the staged mask chooses among them.
macro_rules! dispatch {
    ($sym:expr, $words:expr, $mask:expr, $ret:ty) => {{
        let mut cursor = crate::word::Cursor {
            words: $words,
            at: 0,
        };
        let address = $sym.address();
        match ($words.len(), $mask) {
            (0, 0) => {
                let f: unsafe extern "C" fn() -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f() }
            },
            (1, 0) => {
                let f: unsafe extern "C" fn(i64) -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp()) }
            },
            (1, 1) => {
                let f: unsafe extern "C" fn(f64) -> $ret = unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp()) }
            },
            (2, 0) => {
                let f: unsafe extern "C" fn(i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp()) }
            },
            (2, 1) => {
                let f: unsafe extern "C" fn(f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp()) }
            },
            (2, 2) => {
                let f: unsafe extern "C" fn(i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp()) }
            },
            (2, 3) => {
                let f: unsafe extern "C" fn(f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp()) }
            },
            (3, 0) => {
                let f: unsafe extern "C" fn(i64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.gp()) }
            },
            (3, 1) => {
                let f: unsafe extern "C" fn(f64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.gp()) }
            },
            (3, 2) => {
                let f: unsafe extern "C" fn(i64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.gp()) }
            },
            (3, 3) => {
                let f: unsafe extern "C" fn(f64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.gp()) }
            },
            (3, 4) => {
                let f: unsafe extern "C" fn(i64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.fp()) }
            },
            (3, 5) => {
                let f: unsafe extern "C" fn(f64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.fp()) }
            },
            (3, 6) => {
                let f: unsafe extern "C" fn(i64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.fp()) }
            },
            (3, 7) => {
                let f: unsafe extern "C" fn(f64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.fp()) }
            },
            (4, 0) => {
                let f: unsafe extern "C" fn(i64, i64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.gp(), cursor.gp()) }
            },
            (4, 1) => {
                let f: unsafe extern "C" fn(f64, i64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.gp(), cursor.gp()) }
            },
            (4, 2) => {
                let f: unsafe extern "C" fn(i64, f64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.gp(), cursor.gp()) }
            },
            (4, 3) => {
                let f: unsafe extern "C" fn(f64, f64, i64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.gp(), cursor.gp()) }
            },
            (4, 4) => {
                let f: unsafe extern "C" fn(i64, i64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.fp(), cursor.gp()) }
            },
            (4, 5) => {
                let f: unsafe extern "C" fn(f64, i64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.fp(), cursor.gp()) }
            },
            (4, 6) => {
                let f: unsafe extern "C" fn(i64, f64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.fp(), cursor.gp()) }
            },
            (4, 7) => {
                let f: unsafe extern "C" fn(f64, f64, f64, i64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.fp(), cursor.gp()) }
            },
            (4, 8) => {
                let f: unsafe extern "C" fn(i64, i64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.gp(), cursor.fp()) }
            },
            (4, 9) => {
                let f: unsafe extern "C" fn(f64, i64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.gp(), cursor.fp()) }
            },
            (4, 10) => {
                let f: unsafe extern "C" fn(i64, f64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.gp(), cursor.fp()) }
            },
            (4, 11) => {
                let f: unsafe extern "C" fn(f64, f64, i64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.gp(), cursor.fp()) }
            },
            (4, 12) => {
                let f: unsafe extern "C" fn(i64, i64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.gp(), cursor.fp(), cursor.fp()) }
            },
            (4, 13) => {
                let f: unsafe extern "C" fn(f64, i64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.gp(), cursor.fp(), cursor.fp()) }
            },
            (4, 14) => {
                let f: unsafe extern "C" fn(i64, f64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.gp(), cursor.fp(), cursor.fp(), cursor.fp()) }
            },
            _ => {
                let f: unsafe extern "C" fn(f64, f64, f64, f64) -> $ret =
                    unsafe { std::mem::transmute(address) };
                unsafe { f(cursor.fp(), cursor.fp(), cursor.fp(), cursor.fp()) }
            },
        }
    }};
}

pub(crate) use dispatch;

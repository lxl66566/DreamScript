//! The match/pattern lowering vocabulary: `if`/`match` arm dispatch,
//! pattern binding over a scrutinee, and member/word test emission.

use super::*;

mod bind;
mod control;

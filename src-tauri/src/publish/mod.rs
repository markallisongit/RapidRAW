// Nothing calls into this module yet: the SmugMug client that consumes the
// signer lands in a later change. Until then every item here is dead code as
// far as the binary is concerned, though the unit tests exercise all of it.
#![allow(dead_code)]

pub mod oauth1;

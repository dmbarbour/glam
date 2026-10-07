//! Hash maps for keys the runtime allocates itself.
//!
//! Node, port, copy, fan-site, work, wait, session and task identities come
//! from runtime counters, so a Glam program cannot choose them to provoke
//! collisions. These maps use foldhash's fast, fixed-seed hasher instead of
//! the standard library's SipHash. It is far cheaper for small integer keys,
//! and its fixed seed makes iteration order reproducible.
//!
//! Keys that a Glam program can influence, such as dictionary keys and names,
//! keep the standard `RandomState`.

pub(crate) type TrustedState = foldhash::fast::FixedState;
pub(crate) type TrustedHashMap<K, V> = std::collections::HashMap<K, V, TrustedState>;
pub(crate) type TrustedHashSet<K> = std::collections::HashSet<K, TrustedState>;

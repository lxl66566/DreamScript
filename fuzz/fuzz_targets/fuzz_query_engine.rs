#![no_main]
// Property: the query engine never panics under arbitrary edit sequences
// over synthetic block-structured files, bumps its revision exactly once
// per real content change, and returns the same values as a from-scratch
// replay of the same inputs. The full invariant set (compute-count budgets,
// block-grain renewal accounting, freshness) runs seeded under nextest in
// crates/ds-query/tests/invariants.rs; this target adds coverage feedback.

use std::sync::Arc;

use ds_diag::FileId;
use ds_query::{
    BlockId, BlockSource, Ctx, Db, DefId, ParseFile, Query, QueryError, QueryKey, QueryKind,
    QueryValue,
};
use libfuzzer_sys::fuzz_target;

/// Downstream marker over the placeholder TypeOf key: DefId(i) reads block
/// i of file 0, chaining one block-derived query below BlockSource.
struct TypeOf;

impl Query for TypeOf {
    type Key = DefId;
    type Value = Arc<str>;

    fn make_key(key: DefId) -> QueryKey {
        QueryKey::TypeOf(key)
    }

    fn extract(value: QueryValue) -> Option<Self::Value> {
        match value {
            QueryValue::BlockSource(text) => Some(text),
            _ => None,
        }
    }
}

fn type_of_block(ctx: &mut Ctx<'_>, key: QueryKey) -> Result<QueryValue, QueryError> {
    let QueryKey::TypeOf(def) = key else {
        return Err(QueryError::KindMismatch(key));
    };
    let text = ctx.get::<BlockSource>(BlockId {
        file: FileId::new(0),
        index: def.as_u32(),
    })?;
    Ok(QueryValue::BlockSource(text))
}

/// Ground truth of the two synthetic files: block bodies plus the leading
/// spaces of each separator line (the only edits outside blocks).
struct Model {
    bodies: [Vec<String>; 2],
    spaces: [Vec<u8>; 2],
}

impl Model {
    fn serialize(&self, file: usize) -> String {
        let mut out = String::new();
        for (i, body) in self.bodies[file].iter().enumerate() {
            if i > 0 {
                out.push_str(&" ".repeat(usize::from(self.spaces[file][i - 1])));
                out.push_str("///|\n");
            }
            out.push_str(body);
        }
        out
    }

    /// A fresh db replaying the model (file ids follow registration order).
    fn db(&self) -> Db {
        let mut db = Db::new();
        db.set_compute(QueryKind::TypeOf, type_of_block);
        for (f, name) in ["a.ds", "b.ds"].iter().enumerate() {
            db.add_file(*name, &self.serialize(f));
        }
        db
    }
}

/// The aged db must agree with a from-scratch replay on every live key.
fn compare_with_replay(db: &mut Db, replay: &mut Db, model: &Model) {
    for f in 0_u32..2 {
        let file = FileId::new(f);
        assert_eq!(
            db.get::<ParseFile>(file).unwrap(),
            replay.get::<ParseFile>(file).unwrap()
        );
        for i in 0..model.bodies[f as usize].len() as u32 {
            let block = BlockId { file, index: i };
            assert_eq!(
                db.get::<BlockSource>(block).unwrap(),
                replay.get::<BlockSource>(block).unwrap()
            );
            if f == 0 {
                assert_eq!(
                    db.get::<TypeOf>(DefId::new(i)).unwrap(),
                    replay.get::<TypeOf>(DefId::new(i)).unwrap()
                );
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut model = Model {
        bodies: [
            vec![
                "let a = 0;\n".to_owned(),
                "let b = 1;\n".to_owned(),
                "let c = 2;\n".to_owned(),
            ],
            vec!["let d = 3;\n".to_owned(), "let e = 4;\n".to_owned()],
        ],
        spaces: [vec![0, 0], vec![0]],
    };
    let mut db = model.db();
    for (step, &byte) in data.iter().enumerate().take(256) {
        let file = usize::from(byte & 1);
        let old = model.serialize(file);
        match byte % 7 {
            0..=2 => {
                let at = usize::from(byte >> 3) % model.bodies[file].len();
                model.bodies[file][at] = format!("let v{step}_{byte} = {};\n", byte % 97);
            },
            3 => {
                if let Some(spaces) = model.spaces[file].first_mut() {
                    *spaces ^= 2; // indent/unindent a separator: layout shift
                }
            },
            4 => {
                if model.bodies[file].len() < 8 {
                    let at = usize::from(byte >> 3) % model.bodies[file].len();
                    let body = model.bodies[file][at].clone();
                    let split = usize::from(byte >> 5) % (body.matches('\n').count() + 1);
                    let mut head = String::new();
                    let mut tail = String::new();
                    for (line, chunk) in body.split_inclusive('\n').enumerate() {
                        if line < split {
                            head.push_str(chunk);
                        } else {
                            tail.push_str(chunk);
                        }
                    }
                    model.bodies[file][at] = head;
                    model.bodies[file].insert(at + 1, tail);
                    model.spaces[file].insert(at, 0);
                }
            },
            5 if model.bodies[file].len() > 1 => {
                let at = usize::from(byte >> 3) % (model.bodies[file].len() - 1);
                let tail = model.bodies[file].remove(at + 1);
                model.bodies[file][at] += &tail;
                model.spaces[file].remove(at);
            },
            _ => {}, // no-op edit: same content pushed again
        }
        let rev = db.revision().as_u64();
        let new = model.serialize(file);
        let changed = u64::from(new != old);
        db.set_file_content(FileId::new(file as u32), &new).unwrap();
        assert_eq!(db.revision().as_u64() - rev, changed);
        if step % 16 == 15 {
            compare_with_replay(&mut db, &mut model.db(), &model);
        }
    }
    compare_with_replay(&mut db, &mut model.db(), &model);
});

use super::*;
use bson::{doc, oid::ObjectId};

fn v(text: &str) -> Bson {
    parse_value(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn err(text: &str) -> ParseError {
    parse_value(text).expect_err(text)
}

fn cmd(text: &str) -> MongoCommand {
    let mut all = parse_script(text);
    assert_eq!(all.len(), 1, "{text}");
    all.remove(0)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .command
}

fn cmd_err(text: &str) -> ParseError {
    let mut all = parse_script(text);
    assert_eq!(all.len(), 1, "{text}");
    all.remove(0).expect_err(text)
}

// --- values ---------------------------------------------------------------

#[test]
fn objects_accept_bare_quoted_and_operator_keys_and_trailing_commas() {
    assert_eq!(
        v(r#"{a: 1, "b.c": 2, $gt: 3, 'x': 4, }"#),
        Bson::Document(doc! { "a": 1, "b.c": 2, "$gt": 3, "x": 4 })
    );
    assert_eq!(v("[1, 2, ]"), Bson::Array(vec![1.into(), 2.into()]));
    assert_eq!(v("{}"), Bson::Document(doc! {}));
    assert_eq!(v("[]"), Bson::Array(vec![]));
    assert_eq!(
        v("{a: {b: [true, false, null]}}"),
        Bson::Document(doc! { "a": { "b": [true, false, Bson::Null] } })
    );
}

#[test]
fn numbers_pick_int32_int64_or_double() {
    assert_eq!(v("42"), Bson::Int32(42));
    assert_eq!(v("-42"), Bson::Int32(-42));
    assert_eq!(v("+7"), Bson::Int32(7));
    assert_eq!(v("2147483647"), Bson::Int32(i32::MAX));
    assert_eq!(v("2147483648"), Bson::Int64(2_147_483_648));
    assert_eq!(v("-9223372036854775808"), Bson::Int64(i64::MIN));
    assert_eq!(v("99999999999999999999"), Bson::Double(1e20));
    assert_eq!(v("1.5"), Bson::Double(1.5));
    assert_eq!(v("1.0"), Bson::Double(1.0));
    assert_eq!(v("1e3"), Bson::Double(1000.0));
    assert_eq!(v("-2.5E-3"), Bson::Double(-0.0025));
    assert_eq!(v(".5"), Bson::Double(0.5));
    assert_eq!(v("0x1F"), Bson::Int32(31));
    assert_eq!(v("Infinity"), Bson::Double(f64::INFINITY));
    assert_eq!(v("-Infinity"), Bson::Double(f64::NEG_INFINITY));
    assert!(matches!(v("NaN"), Bson::Double(f) if f.is_nan()));
    assert!(parse_value("12abc").is_err());
    assert!(parse_value("-").is_err());
}

#[test]
fn strings_and_escapes() {
    assert_eq!(
        v(r#""a\nb\tc\rd\be\ff""#),
        Bson::String("a\nb\tc\rd\u{8}e\u{c}f".into())
    );
    assert_eq!(v(r#""\\ \' \" \/""#), Bson::String("\\ ' \" /".into()));
    assert_eq!(v(r#"'it\'s "x"'"#), Bson::String("it's \"x\"".into()));
    assert_eq!(v(r#""é\x41""#), Bson::String("éA".into()));
    assert_eq!(v(r#""😀""#), Bson::String("😀".into()));
    assert_eq!(v(r#""héllo 😀""#), Bson::String("héllo 😀".into()));
    let e = err(r#""abc"#);
    assert_eq!(e.offset, 0);
    assert!(e.message.contains("non fermée"), "{}", e.message);
    let e = err(r#""a\qb""#);
    assert_eq!(e.offset, 2);
    assert!(parse_value(r#""\x4""#).is_err());
}

#[test]
fn every_constructor() {
    let hex = "65a1b2c3d4e5f60718293a4b";
    let oid = ObjectId::parse_str(hex).unwrap();
    assert_eq!(v(&format!("ObjectId(\"{hex}\")")), Bson::ObjectId(oid));
    assert_eq!(v(&format!("new ObjectId('{hex}')")), Bson::ObjectId(oid));
    assert!(matches!(v("ObjectId()"), Bson::ObjectId(_)));
    let date = bson::DateTime::from_millis(1_704_164_645_678);
    assert_eq!(
        v(r#"ISODate("2024-01-02T03:04:05.678Z")"#),
        Bson::DateTime(date)
    );
    assert_eq!(
        v(r#"new Date("2024-01-02T04:04:05.678+01:00")"#),
        Bson::DateTime(date)
    );
    assert_eq!(v("Date(1704164645678)"), Bson::DateTime(date));
    assert_eq!(
        v(r#"ISODate("2024-01-02")"#),
        Bson::DateTime(bson::DateTime::from_millis(1_704_153_600_000))
    );
    assert!(matches!(v("new Date()"), Bson::DateTime(_)));
    assert_eq!(v("NumberLong(5)"), Bson::Int64(5));
    assert_eq!(
        v(r#"NumberLong("9223372036854775807")"#),
        Bson::Int64(i64::MAX)
    );
    assert_eq!(v("NumberInt(5)"), Bson::Int32(5));
    assert_eq!(v(r#"NumberInt("7")"#), Bson::Int32(7));
    assert!(parse_value("NumberInt(3000000000)").is_err());
    assert_eq!(
        v(r#"NumberDecimal("1.10")"#),
        Bson::Decimal128("1.10".parse().unwrap())
    );
    assert_eq!(
        v(r#"Decimal128("-3")"#),
        Bson::Decimal128("-3".parse().unwrap())
    );
    assert_eq!(
        v(r#"UUID("01234567-89ab-cdef-0123-456789abcdef")"#),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: decode_hex("0123456789abcdef0123456789abcdef").unwrap()
        })
    );
    assert_eq!(
        v(r#"BinData(0, "AQID")"#),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![1, 2, 3]
        })
    );
    assert_eq!(
        v("Timestamp(10, 2)"),
        Bson::Timestamp(Timestamp {
            time: 10,
            increment: 2
        })
    );
    assert_eq!(
        v("Timestamp({t: 10, i: 2})"),
        Bson::Timestamp(Timestamp {
            time: 10,
            increment: 2
        })
    );
    assert_eq!(v("MinKey()"), Bson::MinKey);
    assert_eq!(v("MaxKey()"), Bson::MaxKey);
    assert_eq!(
        v(r#"RegExp("^a/b", "i")"#),
        make_regex("^a/b", "i").unwrap()
    );
    let e = err("Foo(1)");
    assert!(e.message.contains("Constructeur inconnu"), "{}", e.message);
    let e = err(r#"{a: ObjectId("zz")}"#);
    assert_eq!(e.offset, 4);
}

#[test]
fn extended_json_objects_become_bson_types() {
    let hex = "65a1b2c3d4e5f60718293a4b";
    assert_eq!(
        v(&format!(r#"{{"_id": {{"$oid": "{hex}"}}}}"#)),
        Bson::Document(doc! { "_id": ObjectId::parse_str(hex).unwrap() })
    );
    assert_eq!(v(r#"{"$numberLong": "12"}"#), Bson::Int64(12));
    assert_eq!(
        v(r#"{"$numberDecimal": "1.5"}"#),
        Bson::Decimal128("1.5".parse().unwrap())
    );
    assert_eq!(
        v(r#"{"$date": "2024-01-02T03:04:05.678Z"}"#),
        Bson::DateTime(bson::DateTime::from_millis(1_704_164_645_678))
    );
    assert_eq!(
        v(r#"{"$date": {"$numberLong": "1704164645678"}}"#),
        Bson::DateTime(bson::DateTime::from_millis(1_704_164_645_678))
    );
    assert_eq!(
        v(r#"{"$binary": {"base64": "AQID", "subType": "00"}}"#),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![1, 2, 3]
        })
    );
    assert_eq!(
        v(r#"{"$timestamp": {"t": 1, "i": 2}}"#),
        Bson::Timestamp(Timestamp {
            time: 1,
            increment: 2
        })
    );
    // Query operators are left alone.
    assert_eq!(v("{$gt: 3}"), Bson::Document(doc! { "$gt": 3 }));
    assert_eq!(
        v(r#"{$regex: "a", $options: "i"}"#),
        Bson::Document(doc! { "$regex": "a", "$options": "i" })
    );
    assert!(parse_value(r#"{"$oid": "nope"}"#).is_err());
}

#[test]
fn regex_literals_and_comments() {
    assert_eq!(v(r"/a\/b[/]c/im"), make_regex(r"a\/b[/]c", "im").unwrap());
    assert_eq!(v("/x/mi"), make_regex("x", "im").unwrap());
    assert_eq!(
        v("{a: /^b/} // trailing"),
        Bson::Document(doc! { "a": make_regex("^b", "").unwrap() })
    );
    assert_eq!(
        v("/* c */ {a: /* in */ 1}"),
        Bson::Document(doc! { "a": 1 })
    );
    // No division in values.
    assert!(parse_value("{a: 1 / 2}").is_err());
    assert!(parse_value("/abc").is_err());
    assert!(parse_value("/a/q").is_err());
    assert!(parse_value("/* open").is_err());
}

#[test]
fn errors_carry_their_offset() {
    let e = err("{a: 1, b: }");
    assert_eq!(e.offset, 10);
    let e = err("{a: 1");
    assert_eq!(e.offset, 0);
    assert!(e.message.contains("}"));
    let e = err("[1, 2 3]");
    assert_eq!(e.offset, 6);
    let e = err("{a 1}");
    assert_eq!(e.offset, 3);
    let e = err("1 2");
    assert_eq!(e.offset, 2);
    let e = err("{a: foo}");
    assert_eq!(e.offset, 4);
}

#[test]
fn parse_document_blank_is_empty_and_rejects_scalars() {
    assert_eq!(parse_document("  "), Ok(doc! {}));
    assert_eq!(parse_document("{a: 1}"), Ok(doc! { "a": 1 }));
    assert!(parse_document("1").is_err());
    assert!(parse_document("[1]").is_err());
}

// --- printing ---------------------------------------------------------------

#[test]
fn printing_is_compact_or_indented() {
    let d = Bson::Document(doc! { "a": 1, "b c": [1, "x"], "e": {} , "f": [] });
    assert_eq!(
        to_shell(&d, false),
        r#"{a: 1, "b c": [1, "x"], e: {}, f: []}"#
    );
    assert_eq!(
        to_shell(&d, true),
        "{\n  a: 1,\n  \"b c\": [\n    1,\n    \"x\"\n  ],\n  e: {},\n  f: []\n}"
    );
    assert_eq!(to_shell(&Bson::Double(1.0), false), "1.0");
    assert_eq!(to_shell(&Bson::Int64(1), false), "NumberLong(1)");
    assert_eq!(
        to_shell(&Bson::Int64(i64::MAX), false),
        "NumberLong(\"9223372036854775807\")"
    );
    assert_eq!(
        to_shell(
            &Bson::DateTime(bson::DateTime::from_millis(1_704_164_645_678)),
            false
        ),
        "ISODate(\"2024-01-02T03:04:05.678Z\")"
    );
    assert_eq!(
        to_shell(&make_regex("a/b", "").unwrap(), false),
        "RegExp(\"a/b\")"
    );
    assert_eq!(to_shell(&make_regex("^a", "i").unwrap(), false), "/^a/i");
    assert_eq!(
        to_shell(&Bson::String("é\n\"".into()), false),
        "\"é\\n\\\"\""
    );
}

/// Deterministic generator (no proptest in the tree): xorshift.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn string(&mut self) -> String {
        const ALPHABET: &[&str] = &[
            "a", "Z", "0", " ", "_", "$", ".", "\"", "'", "\\", "/", "\n", "\t", "\r", "\u{1}",
            "\u{8}", "\u{c}", "é", "😀", "{", "}", "[", "]", ":", ",", "*", "\u{2028}",
        ];
        (0..self.below(8))
            .map(|_| ALPHABET[self.below(ALPHABET.len() as u64) as usize])
            .collect()
    }

    fn value(&mut self, depth: u32) -> Bson {
        let kinds = if depth == 0 { 22 } else { 24 };
        match self.below(kinds) {
            0 => Bson::Double(match self.below(6) {
                0 => 1.0,
                1 => -0.5,
                2 => f64::INFINITY,
                3 => f64::NEG_INFINITY,
                4 => 1e300,
                _ => (self.next() as i64) as f64 / 1000.0,
            }),
            1 => Bson::String(self.string()),
            2 => Bson::Boolean(self.below(2) == 0),
            3 => Bson::Null,
            4 => {
                let flags = ["", "i", "im", "imsx"][self.below(4) as usize];
                let pattern = ["^a", "a/b", "", "*x", "[/]", "a\\/b", "a\\", "x\ny", "[a"]
                    [self.below(9) as usize];
                make_regex(pattern, flags).unwrap()
            }
            5 => Bson::Int32(self.next() as i32),
            6 => Bson::Int64(self.next() as i64),
            7 => Bson::Int64(self.below(1000) as i64),
            8 => Bson::Timestamp(Timestamp {
                time: self.next() as u32,
                increment: self.next() as u32,
            }),
            9 => Bson::Binary(Binary {
                subtype: BinarySubtype::Uuid,
                bytes: (0..16).map(|_| self.next() as u8).collect(),
            }),
            10 => Bson::Binary(Binary {
                subtype: BinarySubtype::from([0u8, 5, 0x80, 3][self.below(4) as usize]),
                bytes: (0..self.below(10)).map(|_| self.next() as u8).collect(),
            }),
            11 => Bson::ObjectId(ObjectId::from_bytes(std::array::from_fn(|_| {
                self.next() as u8
            }))),
            12 => Bson::DateTime(bson::DateTime::from_millis(
                (self.next() % 400_000_000_000_000) as i64 - 100_000_000_000_000,
            )),
            13 => Bson::DateTime(bson::DateTime::from_millis(self.next() as i64)),
            14 => Bson::Decimal128(
                [
                    "0",
                    "1.10",
                    "-3.5E+10",
                    "123456789012345678901234567890",
                    "1E-20",
                ][self.below(5) as usize]
                    .parse()
                    .unwrap(),
            ),
            15 => Bson::MinKey,
            16 => Bson::MaxKey,
            17 => Bson::JavaScriptCode(self.string()),
            18 => Bson::Symbol(self.string()),
            19 => Bson::Undefined,
            20 => Bson::JavaScriptCodeWithScope(bson::JavaScriptCodeWithScope {
                code: self.string(),
                scope: doc! { "x": 1, "y": Bson::Int64(2) },
            }),
            21 => Bson::Int32(0),
            22 => Bson::Array((0..self.below(4)).map(|_| self.value(depth - 1)).collect()),
            _ => {
                let mut d = Document::new();
                for _ in 0..self.below(4) {
                    let key = if self.below(2) == 0 {
                        self.string()
                    } else {
                        "k".into()
                    };
                    // A key starting like extended JSON would read back as a type.
                    if !key.starts_with('$') {
                        d.insert(key, self.value(depth - 1));
                    }
                }
                Bson::Document(d)
            }
        }
    }
}

#[test]
fn printing_round_trips_every_type() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for i in 0..4000 {
        let value = rng.value(3);
        for pretty in [false, true] {
            let text = to_shell(&value, pretty);
            let back = parse_value(&text).unwrap_or_else(|e| panic!("#{i} {text}: {e}"));
            assert_eq!(back, value, "#{i} {text}");
        }
    }
}

#[test]
fn nan_round_trips() {
    for f in [f64::NAN, -0.0] {
        let text = to_shell(&Bson::Double(f), false);
        match parse_value(&text).unwrap() {
            Bson::Double(g) => assert!(
                g.to_bits() == f.to_bits() || (g.is_nan() && f.is_nan()),
                "{text}"
            ),
            other => panic!("{other:?}"),
        }
    }
}

// --- units ---------------------------------------------------------------

fn unit_texts(text: &str) -> Vec<&str> {
    units(text).into_iter().map(|r| &text[r]).collect()
}

#[test]
fn units_split_on_semicolons_and_lines() {
    assert_eq!(
        unit_texts("use shop; db.a.find()\n\ndb.b.find({\n  x: 1\n})\n"),
        ["use shop", "db.a.find()", "db.b.find({\n  x: 1\n})"]
    );
    assert_eq!(
        unit_texts("db.a.find()\n  .sort({a: 1})\n  // note\n  .limit(2)\ndb.b.drop()"),
        [
            "db.a.find()\n  .sort({a: 1})\n  // note\n  .limit(2)",
            "db.b.drop()"
        ]
    );
    // Comments, strings and regexes don't split.
    assert_eq!(
        unit_texts("// head\ndb.a.find({s: \"x;y\", r: /;\\//}) /* ; */\n;;"),
        ["db.a.find({s: \"x;y\", r: /;\\//})"]
    );
    assert_eq!(
        unit_texts("/* only\n comments */\n// here"),
        Vec::<&str>::new()
    );
    assert!(units("").is_empty());
    // An unclosed string ends at its line.
    assert_eq!(
        unit_texts("use \"x\ndb.b.drop()"),
        ["use \"x", "db.b.drop()"]
    );
}

// --- commands ---------------------------------------------------------------

#[test]
fn use_and_show() {
    assert_eq!(cmd("use shop"), MongoCommand::Use("shop".into()));
    assert_eq!(cmd("use \"my db\""), MongoCommand::Use("my db".into()));
    assert_eq!(cmd("show dbs"), MongoCommand::ShowDbs);
    assert_eq!(cmd("show databases"), MongoCommand::ShowDbs);
    assert_eq!(cmd("show collections"), MongoCommand::ShowCollections);
    assert!(parse_script("show users")[0].is_err());
    assert!(parse_script("use")[0].is_err());
}

#[test]
fn collection_accessors() {
    let find = |coll: &str| MongoCommand::Find {
        coll: coll.into(),
        filter: doc! {},
        projection: None,
        sort: None,
        skip: None,
        limit: None,
        one: false,
    };
    assert_eq!(cmd("db.users.find()"), find("users"));
    assert_eq!(cmd("db.fs.chunks.find()"), find("fs.chunks"));
    assert_eq!(cmd(r#"db.getCollection("a-b").find()"#), find("a-b"));
    assert_eq!(cmd(r#"db["a b"].find()"#), find("a b"));
    assert_eq!(cmd("db . users . find ( )"), find("users"));
}

#[test]
fn find_and_its_modifiers() {
    assert_eq!(
        cmd("db.u.find({a: 1}, {b: 1}).sort({c: -1}).skip(5).limit(10).toArray()"),
        MongoCommand::Find {
            coll: "u".into(),
            filter: doc! { "a": 1 },
            projection: Some(doc! { "b": 1 }),
            sort: Some(doc! { "c": -1 }),
            skip: Some(5),
            limit: Some(10),
            one: false,
        }
    );
    assert_eq!(
        cmd("db.u.find().projection({_id: 0}).pretty()"),
        MongoCommand::Find {
            coll: "u".into(),
            filter: doc! {},
            projection: Some(doc! { "_id": 0 }),
            sort: None,
            skip: None,
            limit: None,
            one: false,
        }
    );
    assert_eq!(
        cmd("db.u.findOne({a: 1})"),
        MongoCommand::Find {
            coll: "u".into(),
            filter: doc! { "a": 1 },
            projection: None,
            sort: None,
            skip: None,
            limit: None,
            one: true,
        }
    );
    assert_eq!(
        cmd("db.u.find({a: 1}).count()"),
        MongoCommand::Count {
            coll: "u".into(),
            filter: doc! { "a": 1 }
        }
    );
    assert!(parse_script("db.u.findOne().sort({a: 1})")[0].is_err());
    assert!(parse_script("db.u.find().skip(-1)")[0].is_err());
    assert!(parse_script("db.u.find().explain()")[0].is_err());
    assert!(parse_script("db.u.find(1)")[0].is_err());
    assert!(parse_script("db.u.find({}, {}, {})")[0].is_err());
}

#[test]
fn every_collection_method() {
    let c = || "u".to_string();
    assert_eq!(
        cmd("db.u.aggregate([{$match: {a: 1}}, {$count: \"n\"}], {allowDiskUse: true})"),
        MongoCommand::Aggregate {
            coll: c(),
            pipeline: vec![doc! { "$match": { "a": 1 } }, doc! { "$count": "n" }],
            options: Some(doc! { "allowDiskUse": true }),
        }
    );
    assert_eq!(
        cmd("db.u.countDocuments()"),
        MongoCommand::Count {
            coll: c(),
            filter: doc! {}
        }
    );
    assert_eq!(
        cmd("db.u.estimatedDocumentCount()"),
        MongoCommand::EstimatedCount { coll: c() }
    );
    assert_eq!(
        cmd("db.u.distinct(\"city\", {a: 1})"),
        MongoCommand::Distinct {
            coll: c(),
            field: "city".into(),
            filter: doc! { "a": 1 }
        }
    );
    assert_eq!(
        cmd("db.u.insertOne({a: 1})"),
        MongoCommand::InsertOne {
            coll: c(),
            doc: doc! { "a": 1 }
        }
    );
    assert_eq!(
        cmd("db.u.insertMany([{a: 1}, {a: 2}], {ordered: false})"),
        MongoCommand::InsertMany {
            coll: c(),
            docs: vec![doc! { "a": 1 }, doc! { "a": 2 }],
            options: Some(doc! { "ordered": false }),
        }
    );
    assert_eq!(
        cmd("db.u.updateOne({a: 1}, {$set: {b: 2}}, {upsert: true})"),
        MongoCommand::Update {
            coll: c(),
            filter: doc! { "a": 1 },
            update: UpdateSpec::Document(doc! { "$set": { "b": 2 } }),
            many: false,
            upsert: true,
        }
    );
    assert_eq!(
        cmd("db.u.updateMany({}, [{$set: {b: \"$a\"}}])"),
        MongoCommand::Update {
            coll: c(),
            filter: doc! {},
            update: UpdateSpec::Pipeline(vec![doc! { "$set": { "b": "$a" } }]),
            many: true,
            upsert: false,
        }
    );
    assert_eq!(
        cmd("db.u.replaceOne({_id: 1}, {a: 2})"),
        MongoCommand::ReplaceOne {
            coll: c(),
            filter: doc! { "_id": 1 },
            replacement: doc! { "a": 2 },
            upsert: false,
        }
    );
    assert_eq!(
        cmd("db.u.deleteOne({a: 1})"),
        MongoCommand::Delete {
            coll: c(),
            filter: doc! { "a": 1 },
            many: false
        }
    );
    assert_eq!(
        cmd("db.u.deleteMany({})"),
        MongoCommand::Delete {
            coll: c(),
            filter: doc! {},
            many: true
        }
    );
    assert_eq!(
        cmd("db.u.createIndex({a: 1}, {unique: true, expireAfterSeconds: 60, name: \"ix\"})"),
        MongoCommand::CreateIndex {
            coll: c(),
            keys: doc! { "a": 1 },
            options: Some(doc! { "unique": true, "expireAfterSeconds": 60, "name": "ix" }),
        }
    );
    assert_eq!(
        cmd("db.u.dropIndex(\"ix\")"),
        MongoCommand::DropIndex {
            coll: c(),
            index: "ix".into()
        }
    );
    assert_eq!(
        cmd("db.u.dropIndex({a: 1})"),
        MongoCommand::DropIndex {
            coll: c(),
            index: Bson::Document(doc! { "a": 1 })
        }
    );
    assert_eq!(
        cmd("db.u.getIndexes()"),
        MongoCommand::GetIndexes { coll: c() }
    );
    assert_eq!(
        cmd("db.u.drop()"),
        MongoCommand::DropCollection { coll: c() }
    );
    assert_eq!(
        cmd("db.createCollection(\"logs\", {capped: true, size: 1024})"),
        MongoCommand::CreateCollection {
            name: "logs".into(),
            options: Some(doc! { "capped": true, "size": 1024 })
        }
    );
    assert_eq!(cmd("db.dropDatabase()"), MongoCommand::DropDatabase);
    assert_eq!(
        cmd("db.getCollectionNames()"),
        MongoCommand::GetCollectionNames
    );
    assert_eq!(
        cmd("db.runCommand({ping: 1})"),
        MongoCommand::RunCommand {
            command: doc! { "ping": 1 },
            admin: false
        }
    );
    assert_eq!(
        cmd("db.adminCommand({listDatabases: 1})"),
        MongoCommand::RunCommand {
            command: doc! { "listDatabases": 1 },
            admin: true
        }
    );
}

#[test]
fn unknown_methods_and_options_are_errors() {
    let e = cmd_err("db.u.findAndModify({})");
    assert!(
        e.message
            .contains("Méthode non prise en charge : findAndModify()"),
        "{}",
        e.message
    );
    assert!(e.message.contains("db.runCommand"));
    assert_eq!(e.offset, 5);
    let e = cmd_err("db.currentOp()");
    assert!(e.message.contains("db.currentOp()"), "{}", e.message);
    let e = cmd_err("db.u.updateOne({}, {$set: {a: 1}}, {arrayFilters: []})");
    assert!(
        e.message
            .contains("option non prise en charge : arrayFilters"),
        "{}",
        e.message
    );
    let e = cmd_err("db.u.createIndex({a: 1}, {background: true})");
    assert!(e.message.contains("background"));
    let e = cmd_err("db.u.deleteMany()");
    assert!(e.message.contains("filtre"));
    let e = cmd_err("print(1)");
    assert!(e.message.contains("Commande inconnue"));
    let e = cmd_err("db.u.find() x");
    assert!(e.message.contains("inattendu"));
}

#[test]
fn script_errors_are_offset_in_the_script() {
    let all = parse_script("use a\ndb.u.find({a: })");
    assert!(all[0].is_ok());
    let e = all[1].clone().unwrap_err();
    assert_eq!(e.offset, "use a\ndb.u.find({a: ".len());
    let ok = parse_script("db.a.drop(); db.b.drop()");
    assert_eq!(ok[1].as_ref().unwrap().span, 13..24);
}

#[test]
fn dangerous_commands() {
    let d = |text: &str| danger(&cmd(text));
    assert!(d("db.u.deleteMany({})")
        .unwrap()
        .contains("Supprime tous les documents"));
    assert_eq!(d("db.u.deleteMany({a: 1})"), None);
    assert_eq!(d("db.u.deleteOne({})"), None);
    assert!(d("db.u.updateMany({}, {$set: {a: 1}})")
        .unwrap()
        .contains("Modifie tous"));
    assert_eq!(d("db.u.updateMany({a: 1}, {$set: {a: 1}})"), None);
    assert!(d("db.u.drop()").is_some());
    assert!(d("db.dropDatabase()").is_some());
    assert!(d("db.u.dropIndex(\"*\")").is_some());
    assert_eq!(d("db.u.dropIndex(\"a_1\")"), None);
    for c in [
        "drop",
        "dropDatabase",
        "dropIndexes",
        "shutdown",
        "renameCollection",
    ] {
        assert!(d(&format!("db.runCommand({{{c}: 1}})")).is_some(), "{c}");
        assert!(d(&format!("db.adminCommand({{{c}: 1}})")).is_some(), "{c}");
    }
    assert_eq!(d("db.runCommand({ping: 1})"), None);
    assert_eq!(d("db.u.find()"), None);
}

#[test]
fn commands_print_back_to_themselves() {
    let script = r#"use shop
show dbs
show collections
db.u.find({a: {$gt: 1}}, {b: 1}).sort({c: -1}).skip(2).limit(3)
db.u.findOne()
db.getCollection("a-b").aggregate([{$match: {}}], {allowDiskUse: true})
db.fs.chunks.countDocuments({x: NumberLong(5)})
db.u.estimatedDocumentCount()
db.u.distinct("c")
db.u.insertOne({_id: ObjectId("65a1b2c3d4e5f60718293a4b"), d: ISODate("2024-01-02T03:04:05.678Z")})
db.u.insertMany([{a: 1}], {ordered: false})
db.u.updateMany({}, [{$set: {a: 1}}], {upsert: true})
db.u.replaceOne({_id: 1}, {a: 2})
db.u.deleteOne({a: 1})
db.u.createIndex({a: 1}, {unique: true})
db.u.dropIndex({a: 1})
db.u.getIndexes()
db.u.drop()
db.createCollection("logs")
db.dropDatabase()
db.getCollectionNames()
db.runCommand({ping: 1})
db.adminCommand({listDatabases: 1})
db.find.find()"#;
    for parsed in parse_script(script) {
        let parsed = parsed.unwrap();
        let text = command_to_shell(&parsed.command);
        assert_eq!(cmd(&text), parsed.command, "{text}");
    }
    let long = cmd(&format!("db.u.insertOne({{a: \"{}\"}})", "x".repeat(100)));
    let text = command_to_shell(&long);
    assert!(text.contains('\n'));
    assert_eq!(cmd(&text), long);
}

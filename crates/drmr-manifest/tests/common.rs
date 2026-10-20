use drmr_manifest::{
    CheckStatus, ConfigurationError, Document, DocumentId, DocumentSize, Format, ManifestLink,
    NotCheckedReason, Options, Presentation, Profile, estimate_document_sizes, estimate_resources,
    lint_document, lint_presentation,
};
use std::fmt::Write;
use std::num::NonZeroUsize;

#[test]
fn one_dominant_document_runs_inline_while_balanced_documents_admit_parallelism() {
    let mut options = Options::new(Profile::HlsBaseManifest);
    options.workers = NonZeroUsize::new(8);
    let sizes = [
        drmr_manifest::DocumentSize {
            id: DocumentId(1),
            format: Format::Hls,
            byte_count: 100,
        },
        drmr_manifest::DocumentSize {
            id: DocumentId(2),
            format: Format::Hls,
            byte_count: 500_000,
        },
    ];
    let skew = drmr_manifest::estimate_document_sizes(&sizes, &options).unwrap();
    assert_eq!(skew.admitted_workers, 1);
    let mut balanced = sizes;
    balanced[0].byte_count = 500_000;
    assert_eq!(
        drmr_manifest::estimate_document_sizes(&balanced, &options)
            .unwrap()
            .admitted_workers,
        2
    );
    let at_boundary = [
        drmr_manifest::DocumentSize {
            byte_count: 5_000,
            ..sizes[0]
        },
        drmr_manifest::DocumentSize {
            byte_count: 95_000,
            ..sizes[1]
        },
    ];
    assert_eq!(
        drmr_manifest::estimate_document_sizes(&at_boundary, &options)
            .unwrap()
            .admitted_workers,
        1
    );
    let below = [
        drmr_manifest::DocumentSize {
            byte_count: 5_001,
            ..sizes[0]
        },
        drmr_manifest::DocumentSize {
            byte_count: 94_999,
            ..sizes[1]
        },
    ];
    assert_eq!(
        drmr_manifest::estimate_document_sizes(&below, &options)
            .unwrap()
            .admitted_workers,
        2
    );
}

fn dash(bytes: &[u8]) -> Document<'_> {
    Document {
        id: DocumentId(19),
        format: Format::Dash,
        bytes,
        base_uri: None,
    }
}

#[test]
fn xml_parsing_alone_does_not_pass_dash_semantics() {
    let report = lint_document(
        dash(br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"/>"#),
        &Options::new(Profile::DashStaticManifest),
    )
    .unwrap();
    assert!(report.errors > 0);
    assert!(!report.is_clean_for_scope());
}

#[test]
fn memory_exhaustion_is_incomplete_not_a_clean_result() {
    let mut options = Options::new(Profile::DashStaticManifest);
    options.limits.memory_budget_bytes = 1;
    let report = lint_document(dash(b"<MPD/>"), &options).unwrap();
    assert!(report.incomplete);
    assert!(!report.is_clean_for_scope());
}

#[test]
fn metadata_and_bytes_use_identical_admission_and_overflow_is_refused() {
    let bytes = vec![b'x'; 128 * 1024];
    let documents = [dash(&bytes)];
    let sizes = [DocumentSize {
        id: DocumentId(19),
        format: Format::Dash,
        byte_count: bytes.len(),
    }];
    let mut options = Options::new(Profile::DashStaticManifest);
    options.workers = NonZeroUsize::new(8);
    let estimate = estimate_resources(&documents, &options).unwrap();
    assert_eq!(estimate, estimate_document_sizes(&sizes, &options).unwrap());
    assert!(estimate.admitted_workers > 1);
    assert_eq!(estimate.parser_concurrency, 1);
    let overflow = [
        DocumentSize {
            byte_count: usize::MAX,
            ..sizes[0]
        },
        sizes[0],
    ];
    assert_eq!(
        estimate_document_sizes(&overflow, &options)
            .unwrap()
            .admitted_workers,
        0
    );
}

#[test]
fn worker_admission_respects_cpu_memory_and_small_input_limits() {
    let bytes = vec![b'x'; 128 * 1024];
    let documents = [dash(&bytes)];
    let mut options = Options::new(Profile::DashStaticManifest);
    options.workers = NonZeroUsize::new(64);
    options.limits.max_workers = 8;
    let estimate = estimate_resources(&documents, &options).unwrap();
    assert_eq!(estimate.admitted_workers, 8);
    options.limits.memory_budget_bytes = estimate.retained_bytes
        + estimate.parser_bytes_per_worker
        + estimate.semantic_bytes_per_worker * 2;
    assert_eq!(
        estimate_resources(&documents, &options)
            .unwrap()
            .admitted_workers,
        2
    );
    assert_eq!(
        estimate_resources(&[dash(b"<MPD/>")], &options)
            .unwrap()
            .admitted_workers,
        1
    );
}

#[test]
fn input_debug_and_fixed_errors_omit_manifest_values() {
    let document = dash(b"private-key-uri-sentinel");
    let link = ManifestLink {
        from: document.id,
        reference: "private-key-uri-sentinel",
        to: document.id,
    };
    let graph = Presentation {
        entrypoint: document.id,
        documents: &[document],
        links: &[link],
    };
    assert!(!format!("{graph:?}").contains("sentinel"));
    assert!(!format!("{}", ConfigurationError::InvalidLink).contains("sentinel"));
}

#[test]
fn document_line_and_xml_node_limits_are_not_success() {
    let mut options = Options::new(Profile::DashStaticManifest);
    for kind in 0..4 {
        options.limits = drmr_manifest::Limits::default();
        match kind {
            0 => options.limits.max_document_bytes = 1,
            1 => options.limits.max_lines = 1,
            2 => options.limits.max_line_bytes = 1,
            _ => options.limits.max_xml_nodes = 1,
        }
        let report = lint_document(dash(b"<MPD>\n<Period/>\n</MPD>"), &options).unwrap();
        assert!(report.incomplete || report.errors > 0);
        assert!(!report.is_clean_for_scope());
    }
}

#[test]
fn retrieval_base_metadata_is_bounded_before_any_manifest_parsing() {
    let body = b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\ns.ts\n#EXT-X-ENDLIST\n";
    let mut base = "https://manifest.invalid/".to_owned();
    base.extend(std::iter::repeat_n('p', 8192 - base.len()));
    let options = Options::new(Profile::HlsBaseManifest);
    let document = Document {
        id: DocumentId(1),
        format: Format::Hls,
        bytes: body,
        base_uri: Some(&base),
    };
    let with_base = estimate_resources(&[document], &options).unwrap();
    let sizes = [DocumentSize {
        id: document.id,
        format: document.format,
        byte_count: body.len(),
    }];
    assert_eq!(
        with_base,
        estimate_document_sizes(&sizes, &options).unwrap()
    );
    assert!(
        lint_document(document, &options)
            .unwrap()
            .is_clean_for_scope()
    );
    let oversized = format!("{base}p");
    let report = lint_document(
        Document {
            base_uri: Some(&oversized),
            ..document
        },
        &options,
    )
    .unwrap();
    assert!(report.incomplete && report.errors == 0, "{report:?}");
    assert!(report.coverage.iter().all(|item| {
        item.rule_id == "MANIFEST-RESOURCE-LIMIT"
            && item.status == CheckStatus::NotChecked(NotCheckedReason::ResourceLimit)
    }));
}

#[test]
fn foreign_link_metadata_cannot_exceed_the_supplied_document_byte_budget() {
    let body = b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\ns.ts\n#EXT-X-ENDLIST\n";
    let documents = [Document {
        id: DocumentId(1),
        format: Format::Hls,
        bytes: body,
        base_uri: None,
    }];
    for count in [body.len() - 1, body.len(), body.len() + 1] {
        let name = "p".repeat(count);
        let links = [ManifestLink {
            from: DocumentId(1),
            reference: &name,
            to: DocumentId(1),
        }];
        let report = lint_presentation(
            Presentation {
                entrypoint: DocumentId(1),
                documents: &documents,
                links: &links,
            },
            &Options::new(Profile::HlsBaseManifest),
        )
        .unwrap();
        let exhausted = report.coverage.iter().any(|item| {
            item.rule_id == "MANIFEST-RESOURCE-LIMIT"
                && item.status == CheckStatus::NotChecked(NotCheckedReason::ResourceLimit)
        });
        assert_eq!(exhausted, count > body.len(), "{report:?}");
        if exhausted {
            assert!(report.incomplete && report.errors == 0);
            assert_eq!(report.coverage.len(), 1);
        }
    }
}

const XML_LINES: [&str; 8] = [
    "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" minBufferTime=\"PT1S\" mediaPresentationDuration=\"PT1S\">",
    "<Period>",
    "<AdaptationSet>",
    "<SegmentTemplate duration=\"1\" media=\"s-$Number$\"/>",
    "  <!--音--><Representation id=\"v\" bandwidth=\"1\"/>",
    "</AdaptationSet>",
    "</Period>",
    "</MPD>",
];

#[test]
fn xml_cr_crlf_and_lf_lines_share_limits_without_changing_hls_grammar() {
    for ending in ["\r", "\r\n", "\n"] {
        let text = XML_LINES.join(ending);
        let mut options = Options::new(Profile::DashStaticManifest);
        options.limits.max_lines = XML_LINES.len();
        options.limits.max_line_bytes = XML_LINES.iter().map(|line| line.len()).max().unwrap();
        assert!(
            lint_document(dash(text.as_bytes()), &options)
                .unwrap()
                .is_clean_for_scope()
        );
        for lines in [1, XML_LINES.len() - 1] {
            options.limits.max_lines = lines;
            let report = lint_document(dash(text.as_bytes()), &options).unwrap();
            assert!(
                report.incomplete && report.errors == 0,
                "{ending:?}: {report:?}"
            );
            assert!(report.coverage.iter().all(|item| {
                item.rule_id == "MANIFEST-RESOURCE-LIMIT"
                    && item.status == CheckStatus::NotChecked(NotCheckedReason::ResourceLimit)
            }));
        }
        options.limits.max_lines = XML_LINES.len();
        options.limits.max_line_bytes -= 1;
        assert!(
            lint_document(dash(text.as_bytes()), &options)
                .unwrap()
                .incomplete
        );
    }
    let report = lint_document(
        Document {
            id: DocumentId(1),
            format: Format::Hls,
            bytes: b"#EXTM3U\r#EXT-X-TARGETDURATION:1\r#EXTINF:1,\rs.ts\r#EXT-X-ENDLIST\r",
            base_uri: None,
        },
        &Options::new(Profile::HlsBaseManifest),
    )
    .unwrap();
    assert!(report.errors > 0 && !report.is_clean_for_scope());
}

#[test]
fn xml_line_positions_keep_raw_offsets_and_unicode_columns_for_all_line_endings() {
    for endings in [
        ["\r"; 7],
        ["\r\n"; 7],
        ["\n"; 7],
        ["\r", "\r\n", "\n", "\r", "\n", "\r\n", "\r"],
    ] {
        let mut text = String::new();
        for (index, line) in XML_LINES.iter().enumerate() {
            text.push_str(line);
            if let Some(ending) = endings.get(index) {
                text.push_str(ending);
            }
        }
        let text = text.replace("bandwidth=\"1\"", "bandwidth=\"bad\"");
        let report = lint_document(
            dash(text.as_bytes()),
            &Options::new(Profile::DashStaticManifest),
        )
        .unwrap();
        let diagnostic = report
            .diagnostics
            .iter()
            .find(|item| item.rule_id == "DASH-REPRESENTATION-ATTRIBUTES")
            .unwrap();
        let location = diagnostic.location.unwrap();
        assert_eq!(location.line, 5, "{report:?}");
        assert_eq!(location.column, "  <!--音-->".chars().count() + 1);
        assert_eq!(location.span.start, text.find("<Representation").unwrap());
        assert_eq!(
            &text[location.span.start..location.span.end],
            "<Representation id=\"v\" bandwidth=\"bad\"/>"
        );
    }
}

#[test]
fn shared_graph_checks_longest_path_after_a_short_path() {
    let bodies = [
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=10\nc\n#EXT-X-STREAM-INF:BANDWIDTH=20\na\n",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=10\nb\n",
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=10\nc\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\ns\n#EXT-X-ENDLIST\n",
    ];
    let documents: Vec<_> = bodies
        .iter()
        .enumerate()
        .map(|(index, text)| Document {
            id: DocumentId(u32::try_from(index).unwrap()),
            format: Format::Hls,
            bytes: text.as_bytes(),
            base_uri: None,
        })
        .collect();
    let links = [
        ManifestLink {
            from: DocumentId(0),
            reference: "c",
            to: DocumentId(3),
        },
        ManifestLink {
            from: DocumentId(0),
            reference: "a",
            to: DocumentId(1),
        },
        ManifestLink {
            from: DocumentId(1),
            reference: "b",
            to: DocumentId(2),
        },
        ManifestLink {
            from: DocumentId(2),
            reference: "c",
            to: DocumentId(3),
        },
    ];
    let mut options = Options::new(Profile::HlsBaseManifest);
    options.limits.max_depth = 2;
    let report = lint_presentation(
        Presentation {
            entrypoint: DocumentId(0),
            documents: &documents,
            links: &links,
        },
        &options,
    )
    .unwrap();
    assert!(report.coverage.iter().any(|item| {
        item.rule_id == "MANIFEST-GRAPH-VALID"
            && item
                .not_checked_reasons
                .contains(&NotCheckedReason::ResourceLimit)
    }));
}

#[test]
fn many_document_failures_and_truncation_are_identical_for_one_and_many_workers() {
    let mut bodies = Vec::new();
    let mut master = "#EXTM3U\n".to_owned();
    for index in 0..24 {
        write!(
            master,
            "#EXT-X-STREAM-INF:BANDWIDTH={}\nchild{index}\n",
            index + 1
        )
        .unwrap();
    }
    bodies.push(master);
    for _ in 0..24 {
        let mut media = "#EXTM3U\n#EXT-X-TARGETDURATION:0\n".to_owned();
        for _ in 0..300 {
            media.push_str("#EXTINF:1,\nsegment.m4s\n");
        }
        media.push_str("#EXT-X-ENDLIST\n");
        bodies.push(media);
    }
    let documents: Vec<_> = bodies
        .iter()
        .enumerate()
        .map(|(index, text)| Document {
            id: DocumentId(100 - u32::try_from(index).unwrap()),
            format: Format::Hls,
            bytes: text.as_bytes(),
            base_uri: None,
        })
        .collect();
    let names: Vec<_> = (0..24).map(|index| format!("child{index}")).collect();
    let links: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| ManifestLink {
            from: documents[0].id,
            reference: name,
            to: documents[index + 1].id,
        })
        .collect();
    let graph = Presentation {
        entrypoint: documents[0].id,
        documents: &documents,
        links: &links,
    };
    let mut options = Options::new(Profile::HlsBaseManifest);
    options.limits.max_diagnostics = 3;
    options.workers = NonZeroUsize::new(1);
    let mut one = lint_presentation(graph, &options).unwrap();
    options.workers = NonZeroUsize::new(8);
    let mut many = lint_presentation(graph, &options).unwrap();
    assert!(one.errors > 3);
    assert!(one.truncated && one.incomplete);
    assert!(many.workers_used > 1);
    one.workers_used = 0;
    many.workers_used = 0;
    assert_eq!(one, many);
    let mut rules = std::collections::BTreeSet::new();
    assert!(
        many.coverage
            .iter()
            .all(|item| rules.insert((item.document, item.rule_id)))
    );
    assert!(
        many.coverage
            .iter()
            .any(|item| item.status == CheckStatus::Failed)
    );
}

use std::error::Error;

use zbus_xml::{ArgDirection, Node};
use zvariant::Signature;

#[test]
fn serde() -> Result<(), Box<dyn Error>> {
    let example = include_str!("data/sample_object0.xml");
    let node_r = Node::from_reader(example.as_bytes())?;
    let node = Node::try_from(example)?;
    assert_eq!(node, node_r);
    assert_eq!(node.interfaces().len(), 1);
    assert_eq!(node.interfaces()[0].methods().len(), 3);
    assert_eq!(
        node.interfaces()[0].methods()[0].args()[0]
            .direction()
            .unwrap(),
        ArgDirection::In
    );
    assert_eq!(node.nodes().len(), 4);

    let node_str: Node<'_> = example.try_into()?;
    assert_eq!(node_str.interfaces().len(), 1);
    assert_eq!(node_str.nodes().len(), 4);

    let mut writer = Vec::with_capacity(128);
    node.to_writer(&mut writer).unwrap();

    // Round-trip: the written document parses back to an equal tree.
    let written = String::from_utf8(writer)?;
    let reparsed = Node::try_from(written.as_str())?;
    assert_eq!(node, reparsed);

    Ok(())
}

#[test]
fn invalid_arg_type() {
    let input = include_str!("data/invalid_arg_type.xml");
    assert!(matches!(
        Node::try_from(input),
        Err(zbus_xml::Error::Variant(_))
    ));
}

#[test]
fn multi_complete_arg_type() -> Result<(), Box<dyn Error>> {
    let input = r#"
        <!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
        "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
        <node>
            <interface name="org.test.testinterface">
                <method name="testmethod">
                    <arg name="testarg" direction="out" type="tt"/>
                </method>
            </interface>
        </node>
    "#;

    let node = Node::try_from(input)?;
    let arg = &node.interfaces()[0].methods()[0].args()[0];
    let Signature::Structure(fields) = arg.ty().inner() else {
        panic!("expected `tt` to parse as a structure");
    };

    assert_eq!(fields.len(), 2);
    assert_eq!(fields.get(0), Some(&Signature::U64));
    assert_eq!(fields.get(1), Some(&Signature::U64));

    Ok(())
}

#[test]
fn escaped_attributes() -> Result<(), Box<dyn Error>> {
    let input = r#"
        <node>
            <interface name="org.test.testinterface">
                <annotation name="org.test.Escapes" value="&lt;b&gt; &amp; &quot;q&quot; &apos;a&apos; &#65;&#x42;"/>
            </interface>
        </node>
    "#;

    let node = Node::try_from(input)?;
    let annotation = &node.interfaces()[0].annotations()[0];
    assert_eq!(annotation.value(), r#"<b> & "q" 'a' AB"#);

    // Escaping survives a write/parse round-trip.
    let mut writer = Vec::new();
    node.to_writer(&mut writer)?;
    let written = String::from_utf8(writer)?;
    let reparsed = Node::try_from(written.as_str())?;
    assert_eq!(node, reparsed);

    Ok(())
}

#[test]
fn attribute_whitespace_normalization() -> Result<(), Box<dyn Error>> {
    // Literal whitespace in attribute values is normalized to spaces, while whitespace escaped
    // through character references is kept.
    let input = "<node>\n  <interface name=\"org.test.testinterface\">\n    \
                 <annotation name=\"org.test.Ws\" value=\"a\nb\tc\r&#10;&#9;&#13;d\"/>\n  \
                 </interface>\n</node>";

    let node = Node::try_from(input)?;
    let annotation = &node.interfaces()[0].annotations()[0];
    assert_eq!(annotation.value(), "a b c \n\t\rd");

    // The writer escapes whitespace so it survives normalization by any parser.
    let mut writer = Vec::new();
    node.to_writer(&mut writer)?;
    let written = String::from_utf8(writer)?;
    assert!(written.contains("a b c &#10;&#9;&#13;d"));
    let reparsed = Node::try_from(written.as_str())?;
    assert_eq!(node, reparsed);

    Ok(())
}

#[test]
fn crlf_normalizes_to_single_space() -> Result<(), Box<dyn Error>> {
    // A literal CRLF is a single line ending, so attribute-value normalization collapses it to
    // one space (not two); a lone CR or LF likewise yields one space each.
    let input = "<node name=\"a\r\nb\rc\nd\"/>";
    let node = Node::try_from(input)?;
    assert_eq!(node.name(), Some("a b c d"));

    Ok(())
}

#[test]
fn many_attributes() -> Result<(), Box<dyn Error>> {
    // Elements with many attributes parse fine — duplicate detection falls back from a linear
    // scan to a set past a threshold — and duplicates are still caught beyond that threshold.
    let attrs: String = (0..100).map(|i| format!(" a{i}=\"{i}\"")).collect();
    assert!(Node::try_from(format!("<node{attrs}/>").as_str()).is_ok());
    assert!(matches!(
        Node::try_from(format!("<node{attrs} a50=\"dup\"/>").as_str()),
        Err(zbus_xml::Error::Xml(_))
    ));

    Ok(())
}

#[test]
fn doctype_with_quoted_literals() -> Result<(), Box<dyn Error>> {
    // `>` and brackets inside DOCTYPE quoted literals must not terminate the declaration.
    let input = r#"
        <!DOCTYPE node SYSTEM "weird>literal[with]brackets">
        <node>
            <interface name="org.test.testinterface"/>
        </node>
    "#;

    let node = Node::try_from(input)?;
    assert_eq!(node.interfaces().len(), 1);

    Ok(())
}

#[test]
fn ignores_unknown_elements_and_text() -> Result<(), Box<dyn Error>> {
    // Documents in the wild carry foreign elements (with text content, CDATA and comments) that
    // must be skipped, e.g. Telepathy's `tp:docstring`.
    let input = r#"
        <?xml version="1.0" encoding="UTF-8"?>
        <node xmlns:tp="http://telepathy.freedesktop.org/wiki/DbusSpec#extensions-v0">
            <tp:docstring>Some documentation.</tp:docstring>
            <interface name="org.test.testinterface">
                <!-- a comment -->
                <method name="testmethod">
                    <tp:docstring>More <tp:em>documentation</tp:em>.</tp:docstring>
                    <arg name="testarg" direction="out" type="s">
                        <tp:docstring><![CDATA[Even </more> documentation.]]></tp:docstring>
                    </arg>
                </method>
            </interface>
        </node>
    "#;

    let node = Node::try_from(input)?;
    assert_eq!(node.interfaces().len(), 1);
    let method = &node.interfaces()[0].methods()[0];
    assert_eq!(method.args().len(), 1);
    assert_eq!(method.args()[0].name(), Some("testarg"));

    Ok(())
}

#[test]
fn malformed_documents() {
    for input in [
        // Empty document.
        "",
        // Unclosed root element.
        "<node>",
        // Mismatched closing tag.
        "<node><interface name=\"org.test.testinterface\"></node>",
        // Unterminated comment.
        "<node><!-- comment </node>",
        // Missing attribute value quotes.
        "<node name=foo/>",
        // Unknown entity.
        "<node name=\"&unknown;\"/>",
        // Character references to characters invalid in XML.
        "<node name=\"&#0;\"/>",
        "<node name=\"&#x1F;\"/>",
        // Signed character references.
        "<node name=\"&#+65;\"/>",
        "<node name=\"&#x+41;\"/>",
        // Duplicate attribute.
        "<node name=\"a\" name=\"b\"/>",
        // Missing whitespace between attributes.
        "<node name=\"a\"name=\"b\"/>",
    ] {
        assert!(matches!(
            Node::try_from(input),
            Err(zbus_xml::Error::Xml(_))
        ));
    }

    // Missing required attribute.
    let input = "<node><interface name=\"org.test.testinterface\">\
                 <property name=\"foo\" type=\"s\"/></interface></node>";
    assert!(matches!(
        Node::try_from(input),
        Err(zbus_xml::Error::Xml(_))
    ));

    // Invalid interface name.
    let input = "<node><interface name=\"not a valid name\"/></node>";
    assert!(matches!(
        Node::try_from(input),
        Err(zbus_xml::Error::Name(_))
    ));
}

#[test]
fn error_position() {
    // Errors in attribute values point at the offending reference, not past the value.
    let input = r#"<node name="abc&unknown;def"/>"#;
    let Err(zbus_xml::Error::Xml(e)) = Node::try_from(input) else {
        panic!("expected an XML error");
    };
    assert_eq!(e.position(), input.find('&').unwrap());
}

#[test]
fn deeply_nested_documents() -> Result<(), Box<dyn Error>> {
    // The parser iterates over the axes where a document can nest arbitrarily deep, so any
    // depth up to the cap parses even on a small stack. (Recursing instead overflows even
    // multi-MiB stacks well before the cap in debug builds, whose stack frames are large.)
    let deep_nodes = format!("{}{}", "<node>".repeat(1024), "</node>".repeat(1024));
    let deep_foreign = format!(
        "<node><interface name=\"org.test.testinterface\">{}{}</interface></node>",
        "<x>".repeat(1022),
        "</x>".repeat(1022),
    );

    std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || {
            let node = Node::try_from(deep_nodes.as_str()).unwrap();
            let mut depth = 1;
            let mut node = &node;
            while let [child] = node.nodes() {
                node = child;
                depth += 1;
            }
            assert_eq!(depth, 1024);

            Node::try_from(deep_foreign.as_str()).unwrap();
        })?
        .join()
        .expect("parsing deeply nested documents must not overflow the stack");

    // One level past the cap is rejected cleanly.
    let too_deep = format!("{}{}", "<node>".repeat(1025), "</node>".repeat(1025));
    assert!(matches!(
        Node::try_from(too_deep.as_str()),
        Err(zbus_xml::Error::Xml(_))
    ));

    Ok(())
}

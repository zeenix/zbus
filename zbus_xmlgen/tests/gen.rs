use pretty_assertions::assert_eq;
use std::{env, error::Error, io::Write, path::Path};

use zbus_xml::Node;
use zbus_xmlgen::CodeGenerator;

macro_rules! gen_diff {
    ($infile:literal, $outfile:literal) => {{
        let input = include_str!(concat!("data/", $infile));
        let expected = include_str!(concat!("data/", $outfile));
        #[cfg(windows)]
        let expected = expected.replace("\r\n", "\n");
        let node = Node::from_reader(input.as_bytes())?;
        let r#gen = CodeGenerator::new()
            .with_format(true)
            .interface_code(&node.interfaces()[0])?;

        if env::var("TEST_OVERWRITE").is_ok() {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join("data")
                .join($outfile);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(path)?;
            f.write_all(r#gen.as_bytes())?;
            f.flush()?;
            return Ok(());
        }

        assert_eq!(r#gen, expected);
        Ok(())
    }};
}

#[test]
fn sample_object0() -> Result<(), Box<dyn Error>> {
    gen_diff!("sample_object0.xml", "sample_object0.rs")
}

#[test]
fn struct_return() -> Result<(), Box<dyn Error>> {
    gen_diff!("struct_return.xml", "struct_return.rs")
}

#[test]
fn property_setters() -> Result<(), Box<dyn Error>> {
    gen_diff!("property_setters.xml", "property_setters.rs")
}

#[test]
fn telepathy_docstrings() -> Result<(), Box<dyn Error>> {
    gen_diff!("telepathy_docstrings.xml", "telepathy_docstrings.rs")
}

#[test]
#[allow(deprecated)]
fn deprecated_gen_trait() -> Result<(), Box<dyn Error>> {
    // The deprecated `GenTrait` still works, matching `CodeGenerator`.
    let input = include_str!("data/sample_object0.xml");
    let node = Node::from_reader(input.as_bytes())?;
    let interface = &node.interfaces()[0];

    let gen_trait = zbus_xmlgen::GenTrait {
        interface,
        path: None,
        service: None,
        format: true,
    }
    .to_string();
    let code = CodeGenerator::new()
        .with_format(true)
        .interface_code(interface)?;
    assert_eq!(gen_trait, code);

    Ok(())
}

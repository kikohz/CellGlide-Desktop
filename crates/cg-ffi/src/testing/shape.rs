//! A serde walk that refuses the shapes C# can't read as plain records.

use serde::ser::{self, Impossible, Serialize};
use std::fmt;

/// Whether `value` serializes without a tuple, a fixed-size array, a tuple struct or variant,
/// or a newtype variant (`{"busy":true}`). The walk follows serde's own description of the
/// type, so a `Vec<String>` passes and a pair of objects is caught; only the parts present in
/// `value` are walked, so fill in every `Option`. `Err` names the path to the first offender.
pub fn tuple_free<T: Serialize + ?Sized>(value: &T) -> Result<(), String> {
    value
        .serialize(Shape("$".into()))
        .map_err(|Offender(message)| message)
}

#[derive(Debug)]
struct Offender(String);

impl fmt::Display for Offender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Offender {}

impl ser::Error for Offender {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Offender(message.to_string())
    }
}

/// Walks a value; the string is its path.
struct Shape(String);

/// A sequence, map or struct being walked.
struct Parts {
    path: String,
    index: usize,
}

impl Shape {
    fn refuse(self, what: &str) -> Offender {
        Offender(format!("{} is {what}", self.0))
    }

    fn parts(self) -> Parts {
        Parts {
            path: self.0,
            index: 0,
        }
    }
}

type Refused = Impossible<(), Offender>;

impl ser::Serializer for Shape {
    type Ok = ();
    type Error = Offender;
    type SerializeSeq = Parts;
    type SerializeTuple = Refused;
    type SerializeTupleStruct = Refused;
    type SerializeTupleVariant = Refused;
    type SerializeMap = Parts;
    type SerializeStruct = Parts;
    type SerializeStructVariant = Parts;

    fn serialize_bool(self, _: bool) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_i8(self, _: i8) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_i16(self, _: i16) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_i32(self, _: i32) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_i64(self, _: i64) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_u8(self, _: u8) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_u16(self, _: u16) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_u32(self, _: u32) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_u64(self, _: u64) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_f32(self, _: f32) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_f64(self, _: f64) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_char(self, _: char) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_str(self, _: &str) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Offender> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<(), Offender> {
        Ok(())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), Offender> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: &T,
    ) -> Result<(), Offender> {
        Err(self.refuse(&format!("a newtype variant ({variant})")))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Parts, Offender> {
        Ok(self.parts())
    }
    fn serialize_tuple(self, _: usize) -> Result<Refused, Offender> {
        Err(self.refuse("a tuple or a fixed-size array"))
    }
    fn serialize_tuple_struct(self, name: &'static str, _: usize) -> Result<Refused, Offender> {
        Err(self.refuse(&format!("a tuple struct ({name})")))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Refused, Offender> {
        Err(self.refuse(&format!("a tuple variant ({variant})")))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Parts, Offender> {
        Ok(self.parts())
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Parts, Offender> {
        Ok(self.parts())
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Parts, Offender> {
        Ok(Shape(format!("{}.{variant}", self.0)).parts())
    }
}

impl ser::SerializeSeq for Parts {
    type Ok = ();
    type Error = Offender;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Offender> {
        let path = format!("{}[{}]", self.path, self.index);
        self.index += 1;
        value.serialize(Shape(path))
    }
    fn end(self) -> Result<(), Offender> {
        Ok(())
    }
}

impl ser::SerializeMap for Parts {
    type Ok = ();
    type Error = Offender;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Offender> {
        key.serialize(Shape(format!("{}{{key}}", self.path)))
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Offender> {
        value.serialize(Shape(format!("{}{{value}}", self.path)))
    }
    fn end(self) -> Result<(), Offender> {
        Ok(())
    }
}

impl ser::SerializeStruct for Parts {
    type Ok = ();
    type Error = Offender;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Offender> {
        value.serialize(Shape(format!("{}.{name}", self.path)))
    }
    fn end(self) -> Result<(), Offender> {
        Ok(())
    }
}

impl ser::SerializeStructVariant for Parts {
    type Ok = ();
    type Error = Offender;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Offender> {
        value.serialize(Shape(format!("{}.{name}", self.path)))
    }
    fn end(self) -> Result<(), Offender> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::tuple_free;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Item {
        label: String,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    enum Switch {
        Hidden,
        Busy(bool),
    }

    #[derive(Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Tagged {
        One { n: u8 },
    }

    #[derive(Serialize)]
    struct Point(f32, f32);

    fn item() -> Item {
        Item { label: "a".into() }
    }

    #[test]
    fn named_shapes_and_lists_of_values_pass() {
        assert_eq!(tuple_free(&vec!["a".to_string(), "b".to_string()]), Ok(()));
        assert_eq!(tuple_free(&vec![1.5_f64, 2.5]), Ok(()));
        assert_eq!(tuple_free(&vec![vec![item()]]), Ok(()));
        assert_eq!(tuple_free(&Some(Box::new(item()))), Ok(()));
        assert_eq!(tuple_free(&Switch::Hidden), Ok(()));
        assert_eq!(tuple_free(&Tagged::One { n: 1 }), Ok(()));
    }

    #[test]
    fn tuples_fixed_arrays_and_newtype_variants_are_caught() {
        assert_eq!(
            tuple_free(&vec![(item(), item())]),
            Err("$[0] is a tuple or a fixed-size array".into())
        );
        assert!(tuple_free(&["a".to_string(), "b".to_string()]).is_err());
        assert_eq!(
            tuple_free(&Switch::Busy(true)),
            Err("$ is a newtype variant (busy)".into())
        );
        assert!(tuple_free(&Point(1.0, 2.0)).is_err());
    }
}

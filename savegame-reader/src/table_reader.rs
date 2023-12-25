use std::io::{Read, Seek, SeekFrom};

use byteorder::{BigEndian, ReadBytesExt};
use serde::{Serialize, Serializer};
use serde::ser::{SerializeMap, SerializeSeq};

use crate::base_readers::{has_bit, read_gamma, read_str};

#[derive(Debug)]
pub struct TableItem(pub Vec<ParsedField>);

impl Serialize for TableItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let TableItem(fields) = self;

        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for field in fields {
            map.serialize_entry(field.key.as_str(), &field.data)?;
        }
        map.end()
    }
}

#[derive(Debug)]
pub struct ParsedField {
    key: String,
    data: ParsedFieldData,
}

#[derive(Debug)]
pub enum ParsedFieldData {
    Scalar(ParsedFieldContent),
    List(Vec<ParsedFieldContent>),
}

impl Serialize for ParsedFieldData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ParsedFieldData::Scalar(value) => value.serialize(serializer),
            ParsedFieldData::List(values) => {
                let mut list = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    list.serialize_element(value)?;
                }
                list.end()
            }
        }
    }
}

#[derive(Debug)]
pub enum ParsedFieldContent {
    FileEnd,
    I8(i8),
    U8(u8),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    StringId,
    String,
    Struct(TableItem),
}

impl Serialize for ParsedFieldContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ParsedFieldContent::FileEnd => panic!("should never happen"),
            ParsedFieldContent::I8(value) => serializer.serialize_i8(*value),
            ParsedFieldContent::U8(value) => serializer.serialize_u8(*value),
            ParsedFieldContent::I16(value) => serializer.serialize_i16(*value),
            ParsedFieldContent::U16(value) => serializer.serialize_u16(*value),
            ParsedFieldContent::I32(value) => serializer.serialize_i32(*value),
            ParsedFieldContent::U32(value) => serializer.serialize_u32(*value),
            ParsedFieldContent::I64(value) => serializer.serialize_i64(*value),
            ParsedFieldContent::U64(value) => serializer.serialize_u64(*value),
            ParsedFieldContent::StringId => todo!(),
            ParsedFieldContent::String => todo!(),
            ParsedFieldContent::Struct(table_item) => table_item.serialize(serializer),
        }
    }
}

pub fn read_table_header(reader: &mut impl Read) -> Vec<Field> {
    let mut fields = vec![];
    loop {
        let var_type = VarType::from_byte(reader.read_u8().unwrap());
        match var_type {
            None => break,
            Some(var_type) => {
                let key = read_str(reader);
                fields.push(Field {
                    key,
                    var_type,
                    children: None,
                });
            }
        }
    }
    for field in fields.as_mut_slice() {
        if field.var_type.data_type() == DataType::Struct {
            drop(std::mem::replace(
                field,
                Field {
                    key: field.key.clone(),
                    var_type: field.var_type.clone(),
                    children: Some(read_table_header(reader)),
                },
            ))
        }
    }
    fields
}

pub fn read_table(decoder: &mut (impl Read + Seek), fields: Vec<Field>) -> Vec<TableItem> {
    let mut index = 0usize;
    let mut parsed_items: Vec<TableItem> = Vec::new();
    loop {
        let mut size = read_gamma(decoder);
        if size == 0 {
            break;
        }
        size -= 1;

        if size == 0 {
            println!("Object length: {} bytes", size);
            println!("Index: {}", index);
            println!(
                "File location: {}",
                decoder.seek(SeekFrom::Current(0)).unwrap()
            );
        }
        index += 1;

        let parsed_fields: TableItem = TableItem(
            fields
                .iter()
                .map(|field| field.parse_from(decoder))
                .collect(),
        );
        if index == 702 || index == 703 {
            println!("{:#?}", parsed_fields);
        }
        parsed_items.push(parsed_fields);
    }
    parsed_items
}

pub fn read_sparse_table(decoder: &mut impl Read, fields: Vec<Field>) -> Vec<TableItem> {
    let mut index = 0usize;
    let mut parsed_items: Vec<TableItem> = Vec::new();
    loop {
        let mut size = read_gamma(decoder);
        if size == 0 {
            break;
        }
        size -= 1;

        println!("Object length: {} bytes", size);
        index = read_gamma(decoder);
        println!("Index: {}", index);

        let parsed_fields: TableItem = TableItem(
            fields
                .iter()
                .map(|field| field.parse_from(decoder))
                .collect(),
        );
        parsed_items.push(parsed_fields);
    }
    parsed_items
}

#[derive(Debug)]
pub struct Field {
    key: String,
    var_type: VarType,
    children: Option<Vec<Field>>,
}

impl Field {
    pub fn parse_from(&self, reader: &mut impl Read) -> ParsedField {
        let data = match &self.var_type {
            VarType::List(data_type) => {
                let length = read_gamma(reader);
                let mut items = vec![];
                for i in 0..length {
                    let value = if let Some(children) = &self.children {
                        let mut parsed_children = vec![];
                        for child_field in children {
                            parsed_children.push(child_field.parse_from(reader));
                        }
                        ParsedFieldContent::Struct(TableItem(parsed_children))
                    } else {
                        data_type.read_from(reader)
                    };
                    items.push(value);
                }
                ParsedFieldData::List(items)
            }
            VarType::Scalar(data_type) => {
                let content = if let Some(children) = &self.children {
                    let mut parsed_children: Vec<ParsedField> = vec![];
                    for child_field in children {
                        parsed_children.push(child_field.parse_from(reader));
                    }
                    ParsedFieldContent::Struct(TableItem(parsed_children))
                } else {
                    data_type.read_from(reader)
                };
                ParsedFieldData::Scalar(content)
            }
        };

        ParsedField {
            key: self.key.clone(),
            data,
        }
    }
}

#[derive(Debug, Clone)]
enum VarType {
    Scalar(DataType),
    List(DataType),
}

impl VarType {
    fn from_byte(byte: u8) -> Option<VarType> {
        if byte == 0 {
            return None;
        }

        let data_type = DataType::from_byte(byte & 0b0000_1111);
        let has_length_field = has_bit(usize::from(byte), 4);

        if has_length_field {
            Some(VarType::List(data_type))
        } else {
            Some(VarType::Scalar(data_type))
        }
    }

    fn data_type(&self) -> DataType {
        match self {
            VarType::Scalar(data_type) => data_type.clone(),
            VarType::List(data_type) => data_type.clone(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum DataType {
    FileEnd,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    StringId,
    String,
    Struct,
}

impl DataType {
    fn from_byte(byte: u8) -> DataType {
        match byte {
            0 => DataType::FileEnd,
            1 => DataType::I8,
            2 => DataType::U8,
            3 => DataType::I16,
            4 => DataType::U16,
            5 => DataType::I32,
            6 => DataType::U32,
            7 => DataType::I64,
            8 => DataType::U64,
            9 => DataType::StringId,
            10 => DataType::String,
            11 => DataType::Struct,
            _ => panic!("tried to map {} to DataType", byte),
        }
    }

    fn read_from(&self, reader: &mut impl Read) -> ParsedFieldContent {
        match self {
            DataType::I8 => ParsedFieldContent::I8(reader.read_i8().unwrap()),
            DataType::U8 => ParsedFieldContent::U8(reader.read_u8().unwrap()),
            DataType::I16 => ParsedFieldContent::I16(reader.read_i16::<BigEndian>().unwrap()),
            DataType::U16 => ParsedFieldContent::U16(reader.read_u16::<BigEndian>().unwrap()),
            DataType::I32 => ParsedFieldContent::I32(reader.read_i32::<BigEndian>().unwrap()),
            DataType::U32 => ParsedFieldContent::U32(reader.read_u32::<BigEndian>().unwrap()),
            DataType::I64 => ParsedFieldContent::I64(reader.read_i64::<BigEndian>().unwrap()),
            DataType::U64 => ParsedFieldContent::U64(reader.read_u64::<BigEndian>().unwrap()),
            _ => panic!(),
        }
    }
}

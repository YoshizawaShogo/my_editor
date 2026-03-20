pub mod editable;
pub mod large_file;

pub enum Document {
    Editable(editable::EditableDocument),
    LargeFile(large_file::LargeFileDocument),
}

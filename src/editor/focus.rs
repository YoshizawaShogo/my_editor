#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Editor(Side),
    Completion(Side),
    Shell,
    Overlay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Left,
    Right,
}

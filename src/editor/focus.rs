#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Editor(Side),
    Shell,
    Overlay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    Left,
    Right,
}

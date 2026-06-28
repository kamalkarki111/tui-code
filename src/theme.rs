//! VS Code Dark+ inspired colors (RGB).

pub struct Theme;

impl Theme {
    // Activity / chrome
    pub const BG: (u8, u8, u8) = (30, 30, 30);
    pub const SIDEBAR_BG: (u8, u8, u8) = (37, 37, 38);
    pub const SIDEBAR_FG: (u8, u8, u8) = (204, 204, 204);
    pub const SIDEBAR_SEL_BG: (u8, u8, u8) = (9, 71, 113);
    pub const SIDEBAR_SEL_FG: (u8, u8, u8) = (255, 255, 255);
    pub const SIDEBAR_HOVER: (u8, u8, u8) = (42, 45, 46);

    pub const EDITOR_BG: (u8, u8, u8) = (30, 30, 30);
    pub const EDITOR_FG: (u8, u8, u8) = (212, 212, 212);
    pub const LINE_NUM_FG: (u8, u8, u8) = (133, 133, 133);
    pub const LINE_NUM_ACTIVE: (u8, u8, u8) = (200, 200, 200);
    pub const CUR_LINE_BG: (u8, u8, u8) = (40, 40, 40);

    // Syntax (Dark+)
    pub const KEYWORD: (u8, u8, u8) = (86, 156, 214);
    pub const TYPE: (u8, u8, u8) = (78, 201, 176);
    pub const FUNCTION: (u8, u8, u8) = (220, 220, 170);
    pub const STRING: (u8, u8, u8) = (206, 145, 120);
    pub const NUMBER: (u8, u8, u8) = (181, 206, 168);
    pub const COMMENT: (u8, u8, u8) = (106, 153, 85);
    pub const IDENTIFIER: (u8, u8, u8) = (156, 220, 254);
    pub const FIELD: (u8, u8, u8) = (156, 220, 254);
    pub const ATTRIBUTE: (u8, u8, u8) = (156, 220, 254);
    pub const PUNCTUATION: (u8, u8, u8) = (212, 212, 212);

    pub const TITLE_BG: (u8, u8, u8) = (50, 50, 50);
    pub const TITLE_FG: (u8, u8, u8) = (204, 204, 204);
    pub const TITLE_ACTIVE_FG: (u8, u8, u8) = (255, 255, 255);
    pub const TAB_ACTIVE_BG: (u8, u8, u8) = (30, 30, 30);
    pub const TAB_INACTIVE_BG: (u8, u8, u8) = (45, 45, 45);

    pub const STATUS_BG: (u8, u8, u8) = (0, 122, 204);
    pub const STATUS_FG: (u8, u8, u8) = (255, 255, 255);
    pub const STATUS_SEC_BG: (u8, u8, u8) = (0, 100, 170);

    pub const BORDER: (u8, u8, u8) = (60, 60, 60);
    pub const ACCENT: (u8, u8, u8) = (0, 122, 204);
    pub const DIR_FG: (u8, u8, u8) = (86, 156, 214);
    pub const FILE_FG: (u8, u8, u8) = (204, 204, 204);
    pub const MODIFIED: (u8, u8, u8) = (226, 192, 141);
    pub const DIM: (u8, u8, u8) = (110, 110, 110);
}

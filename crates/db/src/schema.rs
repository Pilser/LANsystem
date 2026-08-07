//! Shared graph schema glossary (single source of truth, mirrors `docs/ARCHITECTURE.md` §10).

pub mod labels {
    pub const USER: &str = "User";
    pub const MEDIA_ITEM: &str = "MediaItem";
    pub const ALBUM: &str = "Album";
    pub const LISTING: &str = "Listing";
    pub const LISTING_REVIEW: &str = "ListingReview";
    pub const ARTICLE: &str = "Article";
    pub const MESSAGE: &str = "Message";
}

pub mod edges {
    pub const FOLLOWS: &str = "FOLLOWS";
    pub const BELONGS_TO: &str = "BELONGS_TO";
    pub const RELEASED: &str = "RELEASED";
    pub const SENT_TO: &str = "SENT_TO";
    pub const AUTHORED_BY: &str = "AUTHORED_BY";
    pub const POSTED_BY: &str = "POSTED_BY";
    pub const PLACED_FOR: &str = "PLACED_FOR";
}

pub mod props {
    pub const USER_ID: &str = "userId";
    pub const USERNAME: &str = "username";
    pub const EMAIL: &str = "email";
    pub const AVATAR_KEY: &str = "avatarKey";
    pub const CREATED_AT: &str = "createdAt";
    pub const UPDATED_AT: &str = "updatedAt";
    pub const DELETED_AT: &str = "deletedAt";
    pub const NAME: &str = "name";
    pub const DISPLAY_NAME: &str = "displayName";
    pub const TITLE: &str = "title";
    pub const DURATION_SEC: &str = "durationSec";
    pub const STREAM_KEY: &str = "streamKey";
    pub const RELEASE_YEAR: &str = "releaseYear";
    pub const DESCRIPTION: &str = "description";
    pub const PRICE: &str = "price";
    pub const CURRENCY: &str = "currency";
    pub const STATUS: &str = "status";
    pub const BODY: &str = "body";
    pub const TEXT: &str = "text";
    pub const SENT_AT: &str = "sentAt";
    pub const READ_AT: &str = "readAt";
    pub const LISTING_ID: &str = "listingId";
    pub const MEDIA_ITEM_ID: &str = "mediaItemId";
    pub const ARTICLE_ID: &str = "articleId";
    pub const MESSAGE_ID: &str = "messageId";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeLabel {
    User,
    MediaItem,
    Album,
    Listing,
    ListingReview,
    Article,
    Message,
    /// A label returned by the database that is not part of the shared glossary.
    Unknown,
}

impl NodeLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => labels::USER,
            Self::MediaItem => labels::MEDIA_ITEM,
            Self::Album => labels::ALBUM,
            Self::Listing => labels::LISTING,
            Self::ListingReview => labels::LISTING_REVIEW,
            Self::Article => labels::ARTICLE,
            Self::Message => labels::MESSAGE,
            Self::Unknown => "Unknown",
        }
    }
}

impl From<&str> for NodeLabel {
    fn from(value: &str) -> Self {
        match value {
            labels::USER => Self::User,
            labels::MEDIA_ITEM => Self::MediaItem,
            labels::ALBUM => Self::Album,
            labels::LISTING => Self::Listing,
            labels::LISTING_REVIEW => Self::ListingReview,
            labels::ARTICLE => Self::Article,
            labels::MESSAGE => Self::Message,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeLabel {
    Follows,
    BelongsTo,
    Released,
    SentTo,
    AuthoredBy,
    PostedBy,
    PlacedFor,
    /// An edge name present in the database that is not part of the shared glossary.
    Unknown,
}

impl EdgeLabel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Follows => edges::FOLLOWS,
            Self::BelongsTo => edges::BELONGS_TO,
            Self::Released => edges::RELEASED,
            Self::SentTo => edges::SENT_TO,
            Self::AuthoredBy => edges::AUTHORED_BY,
            Self::PostedBy => edges::POSTED_BY,
            Self::PlacedFor => edges::PLACED_FOR,
            Self::Unknown => "Unknown",
        }
    }
}

impl From<&str> for EdgeLabel {
    fn from(value: &str) -> Self {
        match value {
            edges::FOLLOWS => Self::Follows,
            edges::BELONGS_TO => Self::BelongsTo,
            edges::RELEASED => Self::Released,
            edges::SENT_TO => Self::SentTo,
            edges::AUTHORED_BY => Self::AuthoredBy,
            edges::POSTED_BY => Self::PostedBy,
            edges::PLACED_FOR => Self::PlacedFor,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Property {
    UserId,
    Username,
    Email,
    AvatarKey,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
    Name,
    DisplayName,
    Title,
    DurationSec,
    StreamKey,
    ReleaseYear,
    Description,
    Price,
    Currency,
    Status,
    Body,
    Text,
    SentAt,
    ReadAt,
    ListingId,
    MediaItemId,
    ArticleId,
    MessageId,
    /// A property present in the database that is not part of the shared glossary.
    Unknown,
}

impl Property {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserId => props::USER_ID,
            Self::Username => props::USERNAME,
            Self::Email => props::EMAIL,
            Self::AvatarKey => props::AVATAR_KEY,
            Self::CreatedAt => props::CREATED_AT,
            Self::UpdatedAt => props::UPDATED_AT,
            Self::DeletedAt => props::DELETED_AT,
            Self::Name => props::NAME,
            Self::DisplayName => props::DISPLAY_NAME,
            Self::Title => props::TITLE,
            Self::DurationSec => props::DURATION_SEC,
            Self::StreamKey => props::STREAM_KEY,
            Self::ReleaseYear => props::RELEASE_YEAR,
            Self::Description => props::DESCRIPTION,
            Self::Price => props::PRICE,
            Self::Currency => props::CURRENCY,
            Self::Status => props::STATUS,
            Self::Body => props::BODY,
            Self::Text => props::TEXT,
            Self::SentAt => props::SENT_AT,
            Self::ReadAt => props::READ_AT,
            Self::ListingId => props::LISTING_ID,
            Self::MediaItemId => props::MEDIA_ITEM_ID,
            Self::ArticleId => props::ARTICLE_ID,
            Self::MessageId => props::MESSAGE_ID,
            Self::Unknown => "Unknown",
        }
    }
}

impl From<&str> for Property {
    fn from(value: &str) -> Self {
        match value {
            props::USER_ID => Self::UserId,
            props::USERNAME => Self::Username,
            props::EMAIL => Self::Email,
            props::AVATAR_KEY => Self::AvatarKey,
            props::CREATED_AT => Self::CreatedAt,
            props::UPDATED_AT => Self::UpdatedAt,
            props::DELETED_AT => Self::DeletedAt,
            props::NAME => Self::Name,
            props::DISPLAY_NAME => Self::DisplayName,
            props::TITLE => Self::Title,
            props::DURATION_SEC => Self::DurationSec,
            props::STREAM_KEY => Self::StreamKey,
            props::RELEASE_YEAR => Self::ReleaseYear,
            props::DESCRIPTION => Self::Description,
            props::PRICE => Self::Price,
            props::CURRENCY => Self::Currency,
            props::STATUS => Self::Status,
            props::BODY => Self::Body,
            props::TEXT => Self::Text,
            props::SENT_AT => Self::SentAt,
            props::READ_AT => Self::ReadAt,
            props::LISTING_ID => Self::ListingId,
            props::MEDIA_ITEM_ID => Self::MediaItemId,
            props::ARTICLE_ID => Self::ArticleId,
            props::MESSAGE_ID => Self::MessageId,
            _ => Self::Unknown,
        }
    }
}
//! Shared graph schema glossary (single source of truth, mirrors `docs/ARCHITECTURE.md` §10).

pub mod labels {
    pub const USER: &str = "User";
    pub const TENANT: &str = "Tenant";
    pub const SESSION: &str = "Session";
    pub const MEDIA_ITEM: &str = "MediaItem";
    pub const ALBUM: &str = "Album";
    pub const LISTING: &str = "Listing";
    pub const LISTING_REVIEW: &str = "ListingReview";
    pub const ARTICLE: &str = "Article";
    pub const MESSAGE: &str = "Message";
}

pub mod edges {
    pub const FOLLOWS: &str = "FOLLOWS";
    pub const MANAGES: &str = "MANAGES";
    pub const MEMBER_OF: &str = "MEMBER_OF";
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
    pub const PASSWORD_HASH: &str = "passwordHash";
    pub const ROLE: &str = "role";
    pub const AVATAR_KEY: &str = "avatarKey";
    pub const CREATED_AT: &str = "createdAt";
    pub const UPDATED_AT: &str = "updatedAt";
    pub const DELETED_AT: &str = "deletedAt";
    pub const TENANT_ID: &str = "tenantId";
    pub const TENANT_NAME: &str = "tenantName";
    pub const APPS_ENABLED: &str = "appsEnabled";
    pub const STORAGE_QUOTA_BYTES: &str = "storageQuotaBytes";
    pub const STORAGE_USED_BYTES: &str = "storageUsedBytes";
    pub const BILLING_ENABLED: &str = "billingEnabled";
    pub const AUTH_WEBHOOK_URL: &str = "authWebhookUrl";
    /// Shared flag: a tenant shows in the public directory, a media item is
    /// listed on the tenant's public surface.
    pub const IS_PUBLIC: &str = "isPublic";
    pub const SESSION_ID: &str = "sessionId";
    pub const EXPIRES_AT: &str = "expiresAt";
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
    Tenant,
    Session,
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
            Self::Tenant => labels::TENANT,
            Self::Session => labels::SESSION,
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
            labels::TENANT => Self::Tenant,
            labels::SESSION => Self::Session,
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
    Manages,
    MemberOf,
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
            Self::Manages => edges::MANAGES,
            Self::MemberOf => edges::MEMBER_OF,
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
            edges::MANAGES => Self::Manages,
            edges::MEMBER_OF => Self::MemberOf,
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
    PasswordHash,
    Role,
    AvatarKey,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
    TenantId,
    TenantName,
    AppsEnabled,
    StorageQuotaBytes,
    StorageUsedBytes,
    BillingEnabled,
    AuthWebhookUrl,
    IsPublic,
    SessionId,
    ExpiresAt,
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
            Self::PasswordHash => props::PASSWORD_HASH,
            Self::Role => props::ROLE,
            Self::AvatarKey => props::AVATAR_KEY,
            Self::CreatedAt => props::CREATED_AT,
            Self::UpdatedAt => props::UPDATED_AT,
            Self::DeletedAt => props::DELETED_AT,
            Self::TenantId => props::TENANT_ID,
            Self::TenantName => props::TENANT_NAME,
            Self::AppsEnabled => props::APPS_ENABLED,
            Self::StorageQuotaBytes => props::STORAGE_QUOTA_BYTES,
            Self::StorageUsedBytes => props::STORAGE_USED_BYTES,
            Self::BillingEnabled => props::BILLING_ENABLED,
            Self::AuthWebhookUrl => props::AUTH_WEBHOOK_URL,
            Self::IsPublic => props::IS_PUBLIC,
            Self::SessionId => props::SESSION_ID,
            Self::ExpiresAt => props::EXPIRES_AT,
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
            props::PASSWORD_HASH => Self::PasswordHash,
            props::ROLE => Self::Role,
            props::AVATAR_KEY => Self::AvatarKey,
            props::CREATED_AT => Self::CreatedAt,
            props::UPDATED_AT => Self::UpdatedAt,
            props::DELETED_AT => Self::DeletedAt,
            props::TENANT_ID => Self::TenantId,
            props::TENANT_NAME => Self::TenantName,
            props::APPS_ENABLED => Self::AppsEnabled,
            props::STORAGE_QUOTA_BYTES => Self::StorageQuotaBytes,
            props::STORAGE_USED_BYTES => Self::StorageUsedBytes,
            props::BILLING_ENABLED => Self::BillingEnabled,
            props::AUTH_WEBHOOK_URL => Self::AuthWebhookUrl,
            props::IS_PUBLIC => Self::IsPublic,
            props::SESSION_ID => Self::SessionId,
            props::EXPIRES_AT => Self::ExpiresAt,
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
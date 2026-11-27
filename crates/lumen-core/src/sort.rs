//! Sort orders and filters for the library screens.

use serde::{Deserialize, Serialize};

/// Sort direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

impl SortDir {
    pub fn flip(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }

    /// "↑ Ascending" / "↓ Descending" (the direction button).
    pub fn button_label(self) -> &'static str {
        match self {
            SortDir::Asc => "↑ Ascending",
            SortDir::Desc => "↓ Descending",
        }
    }

    pub fn arrow(self) -> &'static str {
        match self {
            SortDir::Asc => "↑",
            SortDir::Desc => "↓",
        }
    }
}

/// Song sort orders (Songs screen "Sort by").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SongSort {
    #[default]
    Title,
    Artist,
    Album,
    Duration,
    DateModified,
    DateAdded,
    PlayCount,
}

impl SongSort {
    pub const ALL: [SongSort; 7] = [
        SongSort::Title,
        SongSort::Artist,
        SongSort::Album,
        SongSort::Duration,
        SongSort::DateModified,
        SongSort::DateAdded,
        SongSort::PlayCount,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SongSort::Title => "Title",
            SongSort::Artist => "Artist",
            SongSort::Album => "Album",
            SongSort::Duration => "Duration",
            SongSort::DateModified => "Date modified",
            SongSort::DateAdded => "Date added",
            SongSort::PlayCount => "Play count",
        }
    }

    /// Direction a column starts in when first picked: text A→Z, numbers and
    /// dates biggest/newest first.
    pub fn natural_dir(self) -> SortDir {
        match self {
            SongSort::Title | SongSort::Artist | SongSort::Album => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }
}

/// Album sort orders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlbumSort {
    #[default]
    Title,
    Artist,
    Year,
    DateAdded,
    DateModified,
    TrackCount,
}

impl AlbumSort {
    pub const ALL: [AlbumSort; 6] = [
        AlbumSort::Title,
        AlbumSort::Artist,
        AlbumSort::Year,
        AlbumSort::DateAdded,
        AlbumSort::DateModified,
        AlbumSort::TrackCount,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AlbumSort::Title => "Album title",
            AlbumSort::Artist => "Artist",
            AlbumSort::Year => "Release year",
            AlbumSort::DateAdded => "Date added",
            AlbumSort::DateModified => "Date modified",
            AlbumSort::TrackCount => "Track count",
        }
    }

    pub fn natural_dir(self) -> SortDir {
        match self {
            AlbumSort::Title | AlbumSort::Artist => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }
}

/// Artist sort orders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtistSort {
    #[default]
    Name,
    TrackCount,
    AlbumCount,
    TotalPlays,
    RecentlyPlayed,
    DateAdded,
}

impl ArtistSort {
    pub const ALL: [ArtistSort; 6] = [
        ArtistSort::Name,
        ArtistSort::TrackCount,
        ArtistSort::AlbumCount,
        ArtistSort::TotalPlays,
        ArtistSort::RecentlyPlayed,
        ArtistSort::DateAdded,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ArtistSort::Name => "Artist name",
            ArtistSort::TrackCount => "Track count",
            ArtistSort::AlbumCount => "Album count",
            ArtistSort::TotalPlays => "Total plays",
            ArtistSort::RecentlyPlayed => "Recently played",
            ArtistSort::DateAdded => "Date added",
        }
    }

    pub fn natural_dir(self) -> SortDir {
        match self {
            ArtistSort::Name => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }
}

/// Songs screen filter chips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SongFilter {
    #[default]
    All,
    Lossless,
    FlacOnly,
    SyncedLyrics,
    MissingLyrics,
}

impl SongFilter {
    pub const ALL: [SongFilter; 5] = [
        SongFilter::All,
        SongFilter::Lossless,
        SongFilter::FlacOnly,
        SongFilter::SyncedLyrics,
        SongFilter::MissingLyrics,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SongFilter::All => "All",
            SongFilter::Lossless => "Lossless",
            SongFilter::FlacOnly => "FLAC only",
            SongFilter::SyncedLyrics => "Has synced lyrics",
            SongFilter::MissingLyrics => "Missing lyrics",
        }
    }

    pub fn matches(self, t: &crate::Track) -> bool {
        match self {
            SongFilter::All => true,
            SongFilter::Lossless => t.is_lossless(),
            SongFilter::FlacOnly => t.codec == "FLAC",
            SongFilter::SyncedLyrics => t.lyrics.synced(),
            SongFilter::MissingLyrics => !t.lyrics.any(),
        }
    }
}

/// Playlist sort orders ("Custom order" is the playlist's own order; only
/// there can tracks be dragged).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistSort {
    #[default]
    Custom,
    Title,
    Artist,
    DateAdded,
    DateModified,
    Duration,
}

impl PlaylistSort {
    pub const ALL: [PlaylistSort; 6] = [
        PlaylistSort::Custom,
        PlaylistSort::Title,
        PlaylistSort::Artist,
        PlaylistSort::DateAdded,
        PlaylistSort::DateModified,
        PlaylistSort::Duration,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PlaylistSort::Custom => "Custom order",
            PlaylistSort::Title => "Title",
            PlaylistSort::Artist => "Artist",
            PlaylistSort::DateAdded => "Date added to playlist",
            PlaylistSort::DateModified => "Date modified",
            PlaylistSort::Duration => "Duration",
        }
    }

    pub fn natural_dir(self) -> SortDir {
        match self {
            PlaylistSort::Custom | PlaylistSort::Title | PlaylistSort::Artist => SortDir::Asc,
            _ => SortDir::Desc,
        }
    }
}

/// Provides access to the media player's playlists.
///
/// Since D-Bus does not directly support enums, or a **maybe** type, they are described in this interface.
#[proxy(interface = "com.example.Playlists", assume_defaults = true)]
pub trait Playlists {
    /// ActivatePlaylist method
    ///
    /// Starts playing the given playlist.
    ///
    /// It is up to the media player whether this completely replaces the current tracklist, or whether it is merely inserted into the tracklist and the first track starts.
    ///
    /// # Arguments
    ///
    /// * `playlist_id` - The id of the playlist to activate.
    fn activate_playlist(&self, playlist_id: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// GetPlaylists method
    ///
    /// Gets a set of playlists.
    ///
    /// # Arguments
    ///
    /// * `index` - The index of the first playlist to be fetched (according to the ordering).
    /// * `max_count` - The maximum number of playlists to fetch.
    /// * `order` - The ordering that should be used.
    /// * `reverse_order` - Whether the order should be reversed.
    /// * `playlists` - A list of (at most *max_count*) playlists.
    fn get_playlists(
        &self,
        index: u32,
        max_count: u32,
        order: &str,
        reverse_order: bool,
    ) -> zbus::Result<Vec<(zbus::zvariant::OwnedObjectPath, String, String)>>;

    /// PlaylistChanged signal
    ///
    /// Indicates that either the Name or Icon attribute of a playlist has changed.
    ///
    /// Client implementations should be aware that this signal may not be implemented.
    ///
    /// Without this signal, media players have no way to notify clients of a change in the attributes of a playlist.
    ///
    /// # Arguments
    ///
    /// * `playlist` - The playlist which details have changed.
    #[zbus(signal)]
    fn playlist_changed(
        &self,
        playlist: (zbus::zvariant::ObjectPath<'_>, &str, &str),
    ) -> zbus::Result<()>;

    /// Orderings property
    ///
    /// The available orderings. At least one must be offered.
    ///
    /// Media players may not have access to all the data required for some orderings.
    #[zbus(property)]
    fn orderings(&self) -> zbus::Result<Vec<String>>;

    /// PlaylistCount property
    ///
    /// The number of playlists available.
    #[zbus(property)]
    fn playlist_count(&self) -> zbus::Result<u32>;

    /// Undocumented property
    #[zbus(property)]
    fn undocumented(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_undocumented(&self, value: &str) -> zbus::Result<()>;
}

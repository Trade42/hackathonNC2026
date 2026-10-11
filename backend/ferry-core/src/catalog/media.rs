//! Public image URLs for restaurants and menu items.
//!
//! An `image_url` column holds either an absolute URL or a path in the public `media` bucket
//! (spec section 2: public files are at `/storage/v1/object/public/media/{path}`). The contract
//! wants an absolute URL or null, so a path becomes a Supabase URL, and is null when
//! `SUPABASE_URL` is unset.

/// The URL to send for a stored `image_url`, or `None`.
pub fn public_media_url(stored: Option<&str>, supabase_url: Option<&str>) -> Option<String> {
    let stored = stored.map(str::trim).filter(|s| !s.is_empty())?;
    if let Ok(url) = url::Url::parse(stored) {
        return matches!(url.scheme(), "http" | "https").then(|| stored.to_owned());
    }
    let path = stored.trim_start_matches('/');
    if path.split('/').any(|segment| segment == "..") {
        return None;
    }
    let mut url = url::Url::parse(supabase_url?).ok()?;
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(["storage", "v1", "object", "public", "media"])
        .extend(path.split('/'));
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Option<&str> = Some("https://abc.supabase.co");

    #[test]
    fn a_path_becomes_the_public_media_url() {
        assert_eq!(
            public_media_url(Some("restaurants/rst_cosmic.jpg"), BASE).as_deref(),
            Some(
                "https://abc.supabase.co/storage/v1/object/public/media/restaurants/rst_cosmic.jpg"
            )
        );
        assert_eq!(
            public_media_url(Some("/a b.jpg"), BASE).as_deref(),
            Some("https://abc.supabase.co/storage/v1/object/public/media/a%20b.jpg")
        );
        // The config strips the trailing slash, but a base with one still works.
        assert_eq!(
            public_media_url(Some("x.png"), Some("https://abc.supabase.co/")).as_deref(),
            Some("https://abc.supabase.co/storage/v1/object/public/media/x.png")
        );
    }

    #[test]
    fn absolute_urls_pass_through() {
        let url = "https://images.example/cosmic.jpg";
        assert_eq!(public_media_url(Some(url), BASE).as_deref(), Some(url));
        assert_eq!(public_media_url(Some(url), None).as_deref(), Some(url));
    }

    #[test]
    fn null_when_there_is_nothing_absolute_to_send() {
        assert_eq!(public_media_url(None, BASE), None);
        assert_eq!(public_media_url(Some("  "), BASE), None);
        assert_eq!(public_media_url(Some("x.png"), None), None);
        assert_eq!(public_media_url(Some("javascript:alert(1)"), BASE), None);
        assert_eq!(
            public_media_url(Some("../receipts/run_x/r.jpg"), BASE),
            None
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_duckduckgo_challenge_pages() {
        let challenge = "Unfortunately, bots use DuckDuckGo too. Please confirm this search was made by a human.";
        assert!(is_bot_blocked(challenge));
    }

    #[test]
    fn detects_closed_browser_connection_errors() {
        assert!(is_browser_connection_closed_message(
            "Unable to make method calls because underlying connection is closed"
        ));
        assert!(is_browser_connection_closed_message(
            "Transport loop got a timeout while listening for messages; connection closed"
        ));
        assert!(!is_browser_connection_closed_message(
            "Failed waiting for selector '#login': timeout"
        ));
    }

    #[test]
    fn parses_duckduckgo_lite_results_and_decodes_redirects() {
        let html = r#"
            <html><body>
              <table>
                <tr>
                  <td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc" class='result-link'>Rust Programming Language</a></td>
                </tr>
                <tr><td class='result-snippet'>Rust is blazingly fast and memory-efficient.</td></tr>
                <tr>
                  <td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdoc.rust-lang.org%2Fbook%2F&amp;rut=def" class='result-link'>The Rust Programming Language</a></td>
                </tr>
                <tr><td class='result-snippet'>Read the book.</td></tr>
              </table>
            </body></html>
        "#;

        let results = parse_duckduckgo_lite_results(html);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "Rust is blazingly fast and memory-efficient."
        );
    }

    #[test]
    fn parses_brave_results() {
        let html = r#"
            <html><body>
              <div class="snippet" data-type="web">
                <div class="result-content">
                  <a href="https://rust-lang.org/" target="_self">
                    <div class="title search-snippet-title" title="Rust Programming Language">Rust Programming Language</div>
                  </a>
                  <div class="generic-snippet">
                    <div class="content">Rust is blazingly fast and memory-efficient.</div>
                  </div>
                </div>
              </div>
            </body></html>
        "#;

        let results = parse_brave_results(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert_eq!(
            results[0].snippet,
            "Rust is blazingly fast and memory-efficient."
        );
    }
}

//! Use case: search the packages available in nixpkgs.

use super::error::AppError;
use super::ports::{Notice, Question, Services};
use crate::domain::package::{Package, PackageSource};
use crate::domain::resolution::{likely_typos, rank_results};

/// Suggestions shown for a likely typo.
const MAX_SUGGESTIONS: usize = 5;

/// Results of a search, best match first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResults {
    pub term: String,
    pub packages: Vec<Package>,
    /// Whether the results are from the revision the system's configuration locks.
    pub pinned: bool,
}

/// Searches names and descriptions.
pub struct SearchPackages<'a> {
    pub services: Services<'a>,
}

impl SearchPackages<'_> {
    /// Runs the search, or returns `None` if the user preferred not to, given a likely
    /// typo: a full search is slow, and a typo can be answered from the attribute names.
    /// Having no results is a valid outcome; a failed search is an
    /// error, with the reason Nix reported.
    pub fn execute(&self, term: &str) -> Result<Option<SearchResults>, AppError> {
        let catalog = self
            .services
            .catalogs
            .catalog(&PackageSource::Nixpkgs)
            .map_err(AppError::SearchFailed)?;
        // Only an optimization: if the names cannot be read, the search reports why.
        let typos = catalog
            .names()
            .map(|names| likely_typos(&names, term, MAX_SUGGESTIONS))
            .unwrap_or_default();
        if !typos.is_empty() {
            self.services.ui.notify(&Notice::DidYouMean(typos))?;
            let question = Question::RunFullSearch(term.to_owned());
            if !self.services.ui.confirm(&question)? {
                return Ok(None);
            }
        }
        let packages = catalog.search(term).map_err(AppError::SearchFailed)?;
        Ok(Some(SearchResults {
            term: term.to_owned(),
            packages: rank_results(packages, term),
            pinned: catalog.is_pinned(),
        }))
    }

    /// Completes `page` with the metadata a search does not report. If that fails,
    /// the page is returned as it was, and its missing fields stay unknown.
    pub fn with_details(&self, page: &[Package]) -> Vec<Package> {
        let attrs: Vec<String> = page
            .iter()
            .map(|package| package.identity.attr.clone())
            .collect();
        let details = self
            .services
            .catalogs
            .catalog(&PackageSource::Nixpkgs)
            .and_then(|catalog| catalog.metadata(&attrs))
            .unwrap_or_default();
        page.iter()
            .map(|package| {
                details
                    .get(&package.identity.attr)
                    .cloned()
                    .unwrap_or_else(|| package.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::error::SearchError;
    use crate::application::test_support::{Fixture, package};

    fn search(fixture: &Fixture, term: &str) -> Result<SearchResults, AppError> {
        SearchPackages {
            services: fixture.services(),
        }
        .execute(term)
        .map(|results| results.expect("the search should have run"))
    }

    #[test]
    fn empty_results_are_not_an_error() {
        let fixture = Fixture::new("[ ]");
        assert!(search(&fixture, "nope").unwrap().packages.is_empty());
    }

    #[test]
    fn results_are_ranked() {
        let fixture = Fixture::new("[ ]")
            .with_package("foobar")
            .with_package("foo");
        let results = search(&fixture, "foo").unwrap();
        assert_eq!(results.packages[0].identity.attr, "foo");
    }

    #[test]
    fn likely_typo_needs_consent_for_a_full_search() {
        let fixture = Fixture::new("[ ]").with_package("firefox");
        let use_case = SearchPackages {
            services: fixture.services(),
        };
        assert_eq!(use_case.execute("firefx").unwrap(), None);
        assert_eq!(fixture.searches(), 0);
        assert!(
            fixture
                .notices()
                .contains(&Notice::DidYouMean(vec!["firefox".to_owned()]))
        );

        let fixture = Fixture::new("[ ]")
            .with_package("firefox")
            .answering(&[true]);
        assert!(search(&fixture, "firefx").is_ok());
        assert_eq!(fixture.searches(), 1);
    }

    #[test]
    fn partial_names_search_without_asking() {
        let fixture = Fixture::new("[ ]").with_package("firefox");
        search(&fixture, "fire").unwrap();
        assert_eq!(fixture.searches(), 1);
        assert_eq!(fixture.confirmations(), 0);
    }

    #[test]
    fn failure_is_reported_as_it_happened() {
        let fixture = Fixture::new("[ ]").failing_catalog();
        assert!(matches!(
            search(&fixture, "foo"),
            Err(AppError::SearchFailed(SearchError::Failed(_)))
        ));
    }

    #[test]
    fn details_complete_the_page_and_failures_leave_it_alone() {
        let mut detailed = package("foo");
        detailed.metadata.license = Some("MIT".to_owned());
        let fixture = Fixture::new("[ ]").with(detailed);
        let use_case = SearchPackages {
            services: fixture.services(),
        };
        let page = [package("foo")];
        assert_eq!(
            use_case.with_details(&page)[0].metadata.license.as_deref(),
            Some("MIT")
        );

        let fixture = Fixture::new("[ ]").failing_catalog();
        let use_case = SearchPackages {
            services: fixture.services(),
        };
        assert_eq!(use_case.with_details(&page), page);
    }
}

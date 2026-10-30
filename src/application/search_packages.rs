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
        if term.trim().is_empty() {
            return Err(AppError::EmptySearchTerm);
        }
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
            if !self.services.ui.confirm(&Question::SearchDespiteTypo)? {
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

    /// Completes `page` with the metadata a search does not report. If that fails, the
    /// page is returned as it was, with its missing fields unknown, and the reason.
    pub fn with_details(&self, page: &[Package]) -> (Vec<Package>, Option<String>) {
        let attrs: Vec<String> = page
            .iter()
            .map(|package| package.identity.attr.clone())
            .collect();
        let queried = self
            .services
            .catalogs
            .catalog(&PackageSource::Nixpkgs)
            .and_then(|catalog| {
                let details = catalog.metadata(&attrs)?;
                Ok((details, catalog.warning()))
            });
        let (details, warning) = match queried {
            Ok(result) => result,
            Err(error) => (
                Default::default(),
                Some(format!("could not read package details: {error}")),
            ),
        };
        let packages = page
            .iter()
            .map(|package| {
                details
                    .get(&package.identity.attr)
                    .cloned()
                    .unwrap_or_else(|| package.clone())
            })
            .collect();
        (packages, warning)
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
    fn an_empty_term_is_refused() {
        let fixture = Fixture::new("[ ]");
        let use_case = SearchPackages {
            services: fixture.services(),
        };
        for term in ["", "  "] {
            assert!(matches!(
                use_case.execute(term),
                Err(AppError::EmptySearchTerm)
            ));
        }
        assert_eq!(fixture.searches(), 0);
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
        let (detailed_page, warning) = use_case.with_details(&page);
        assert_eq!(detailed_page[0].metadata.license.as_deref(), Some("MIT"));
        assert_eq!(warning, None);

        let fixture = Fixture::new("[ ]").failing_catalog();
        let use_case = SearchPackages {
            services: fixture.services(),
        };
        let (same_page, warning) = use_case.with_details(&page);
        assert_eq!(same_page, page);
        assert!(warning.unwrap().contains("network down"));
    }
}

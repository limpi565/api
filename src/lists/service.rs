// Pi-hole: A black hole for Internet advertisements
// (c) 2019 Pi-hole, LLC (https://pi-hole.net)
// Network-wide ad blocking via your own hardware.
//
// API
// List Service (Whitelist, Blacklist, Regexlist)
//
// This file is copyright under the latest version of the EUPL.
// Please see LICENSE file for your rights under this license.

use crate::{
    env::Env,
    ftl::FtlConnectionType,
    lists::{List, ListRepository, ListRepositoryGuard},
    util::{Error, ErrorKind}
};
use failure::ResultExt;
use rocket::{
    request::{self, FromRequest},
    Outcome, Request, State
};
use std::{
    ops::Deref,
    process::{Command, Stdio}
};

#[cfg(test)]
use mock_it::Mock;

/// Describes interactions with the Pi-hole domain lists (whitelist, blacklist,
/// and regexlist)
pub trait ListService {
    /// Add a domain to the list and update FTL and other lists accordingly.
    /// Example: when adding to the whitelist, remove from the blacklist.
    fn add(&self, list: List, domain: &str) -> Result<(), Error>;

    /// Remove a domain from the list and update FTL
    fn remove(&self, list: List, domain: &str) -> Result<(), Error>;

    /// Get all of the domains in the list
    fn get(&self, list: List) -> Result<Vec<String>, Error>;
}

service!(
    ListServiceGuard,
    ListService,
    ListServiceImpl,
    ListServiceMock
);

/// The implementation of `ListService`
pub struct ListServiceImpl<'r> {
    repo: Box<dyn Deref<Target = ListRepository + 'r> + 'r>,
    env: &'r Env,
    ftl: &'r FtlConnectionType
}

impl<'a, 'r> FromRequest<'a, 'r> for ListServiceImpl<'r> {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> request::Outcome<Self, Self::Error> {
        let repo = Box::new(request.guard::<ListRepositoryGuard<'r>>()?);
        let env = request.guard::<State<Env>>()?.inner();
        let ftl = request.guard::<State<FtlConnectionType>>()?.inner();

        Outcome::Success(ListServiceImpl { repo, env, ftl })
    }
}

impl<'r> ListService for ListServiceImpl<'r> {
    fn add(&self, list: List, domain: &str) -> Result<(), Error> {
        match list {
            List::White => {
                // We need to add it to the whitelist and remove it from the
                // blacklist
                self.add_raw(List::White, domain)?;
                self.try_remove_raw(List::Black, domain)?;

                // Since we haven't hit an error yet, reload gravity
                reload_gravity(List::White, &self.env)
            }
            List::Black => {
                // We need to add it to the blacklist and remove it from the
                // whitelist
                self.add_raw(List::Black, domain)?;
                self.try_remove_raw(List::White, domain)?;

                // Since we haven't hit an error yet, reload gravity
                reload_gravity(List::Black, &self.env)
            }
            List::Regex => {
                // We only need to add it to the regex list
                self.add_raw(List::Regex, domain)?;

                // Since we haven't hit an error yet, tell FTL to recompile
                // regex
                self.ftl.connect("recompile-regex")?.expect_eom()
            }
        }
    }

    fn remove(&self, list: List, domain: &str) -> Result<(), Error> {
        match list {
            List::White => {
                self.remove_raw(List::White, domain)?;
                reload_gravity(List::White, &self.env)
            }
            List::Black => {
                self.remove_raw(List::Black, domain)?;
                reload_gravity(List::Black, &self.env)
            }
            List::Regex => {
                self.remove_raw(List::Regex, domain)?;
                self.ftl.connect("recompile-regex")?.expect_eom()
            }
        }
    }

    fn get(&self, list: List) -> Result<Vec<String>, Error> {
        self.repo.get(list)
    }
}

impl<'r> ListServiceImpl<'r> {
    /// Simply add a domain to the list
    fn add_raw(&self, list: List, domain: &str) -> Result<(), Error> {
        // Check if it's a valid domain before doing anything
        if !list.accepts(domain) {
            return Err(Error::from(ErrorKind::InvalidDomain));
        }

        // Check if the domain is already in the list
        if self.repo.contains(list, domain)? {
            return Err(Error::from(ErrorKind::AlreadyExists));
        }

        self.repo.add(list, domain)
    }

    /// Try to remove a domain from the list, but it is not an error if the
    /// domain does not exist
    fn try_remove_raw(&self, list: List, domain: &str) -> Result<(), Error> {
        match self.remove_raw(list, domain) {
            // Pass through successful results
            Ok(_) => Ok(()),
            Err(e) => {
                // Ignore NotFound errors
                if e.kind() == ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Simply remove a domain from the list
    fn remove_raw(&self, list: List, domain: &str) -> Result<(), Error> {
        // Check if it's a valid domain before doing anything
        if !list.accepts(domain) {
            return Err(Error::from(ErrorKind::InvalidDomain));
        }

        // Check if the domain is not in the list
        if !self.repo.contains(list, domain)? {
            return Err(Error::from(ErrorKind::NotFound));
        }

        self.repo.remove(list, domain)
    }
}

#[cfg(test)]
#[derive(Clone)]
pub struct ListServiceMock {
    add: Mock<(List, String), Result<(), Error>>,
    remove: Mock<(List, String), Result<(), Error>>,
    get: Mock<List, Result<Vec<String>, Error>>
}

#[cfg(test)]
impl ListServiceMock {
    pub fn new() -> Self {
        ListServiceMock {
            add: Mock::new(Ok(())),
            remove: Mock::new(Ok(())),
            get: Mock::new(Ok(Vec::new()))
        }
    }
}

#[cfg(test)]
impl ListService for ListServiceMock {
    fn add(&self, list: List, domain: &str) -> Result<(), Error> {
        self.add.called((list, domain.to_owned()))
    }

    fn remove(&self, list: List, domain: &str) -> Result<(), Error> {
        self.remove.called((list, domain.to_owned()))
    }

    fn get(&self, list: List) -> Result<Vec<String>, Error> {
        self.get.called(list)
    }
}

/// Reload Gravity to activate changes in lists
pub fn reload_gravity(list: List, env: &Env) -> Result<(), Error> {
    // Don't actually reload Gravity during testing
    if env.is_test() {
        return Ok(());
    }

    let status = Command::new("sudo")
        .arg("pihole")
        .arg("-g")
        .arg("--skip-download")
        // Based on what list we modified, only reload what is necessary
        .arg(match list {
            List::White => "--whitelist-only",
            List::Black => "--blacklist-only",
            _ => return Err(Error::from(ErrorKind::Unknown))
        })
        // Ignore stdin, stdout, and stderr
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Get the returned status code
        .status()
        .context(ErrorKind::GravityError)?;

    if status.success() {
        Ok(())
    } else {
        Err(Error::from(ErrorKind::GravityError))
    }
}

#[cfg(test)]
mod test {
    use super::List;
    use crate::{
        ftl::FtlConnectionType,
        lists::{ListRepositoryMock, ListService, ListServiceImpl},
        testing::{write_eom, TestEnvBuilder}
    };
    use mock_it::verify;
    use std::collections::HashMap;

    fn get_ftl() -> FtlConnectionType {
        let mut data = Vec::new();
        let mut command_map = HashMap::new();

        write_eom(&mut data);
        command_map.insert("recompile-regex".to_owned(), data);

        FtlConnectionType::Test(command_map)
    }

    /// Test getting the domains for a list
    fn get_test(list: List, domain: &str) {
        let env = TestEnvBuilder::new().build();
        let ftl = get_ftl();
        let repo = ListRepositoryMock::new();

        repo.get
            .given(list)
            .will_return(Ok(vec![domain.to_owned()]));

        let service = ListServiceImpl {
            repo: Box::new(repo.clone()),
            env: &env,
            ftl: &ftl
        };

        assert_eq!(service.get(list).unwrap(), vec![domain.to_owned()]);

        assert!(verify(repo.get.was_called_with(list)))
    }

    /// Test successfully deleting a domain from a list
    fn delete_test(list: List, domain: &str) {
        let env = TestEnvBuilder::new().build();
        let ftl = get_ftl();
        let repo = ListRepositoryMock::new();

        repo.contains
            .given((list, domain.to_owned()))
            .will_return(Ok(true));
        repo.remove
            .given((list, domain.to_owned()))
            .will_return(Ok(()));

        let service = ListServiceImpl {
            repo: Box::new(repo.clone()),
            env: &env,
            ftl: &ftl
        };

        service.remove(list, domain).unwrap();

        assert!(verify(
            repo.remove.was_called_with((list, domain.to_owned()))
        ));
    }

    /// The whitelist is retrieved correctly
    #[test]
    fn get_whitelist() {
        get_test(List::White, "whitelist.com");
    }

    /// The blacklist is retrieved correctly
    #[test]
    fn get_blacklist() {
        get_test(List::Black, "blacklist.com");
    }

    /// The regexlist is retrieved correctly
    #[test]
    fn get_regexlist() {
        get_test(List::Regex, "regex.com");
    }

    /// Adding a domain to the whitelist works when the domain does not exist
    /// in either the whitelist or blacklist
    #[test]
    fn add_whitelist() {
        let env = TestEnvBuilder::new().build();
        let ftl = get_ftl();
        let repo = ListRepositoryMock::new();

        repo.contains
            .given((List::White, "example.com".to_owned()))
            .will_return(Ok(false));
        repo.add
            .given((List::White, "example.com".to_owned()))
            .will_return(Ok(()));
        repo.contains
            .given((List::Black, "example.com".to_owned()))
            .will_return(Ok(false));

        let service = ListServiceImpl {
            repo: Box::new(repo.clone()),
            env: &env,
            ftl: &ftl
        };

        service.add(List::White, "example.com").unwrap();

        assert!(verify(
            repo.add
                .was_called_with((List::White, "example.com".to_owned()))
        ));
    }

    /// Adding a domain to the blacklist works when the domain does not exist
    /// in either the whitelist or blacklist
    #[test]
    fn add_blacklist() {
        let env = TestEnvBuilder::new().build();
        let ftl = get_ftl();
        let repo = ListRepositoryMock::new();

        repo.contains
            .given((List::Black, "example.com".to_owned()))
            .will_return(Ok(false));
        repo.add
            .given((List::Black, "example.com".to_owned()))
            .will_return(Ok(()));
        repo.contains
            .given((List::White, "example.com".to_owned()))
            .will_return(Ok(false));

        let service = ListServiceImpl {
            repo: Box::new(repo.clone()),
            env: &env,
            ftl: &ftl
        };

        service.add(List::Black, "example.com").unwrap();

        assert!(verify(
            repo.add
                .was_called_with((List::Black, "example.com".to_owned()))
        ));
    }

    /// Adding a domain to the regex list works when the domain does not already
    /// exist in the regex list
    #[test]
    fn add_regexlist() {
        let env = TestEnvBuilder::new().build();
        let ftl = get_ftl();
        let repo = ListRepositoryMock::new();

        repo.contains
            .given((List::Regex, "example.com".to_owned()))
            .will_return(Ok(false));
        repo.add
            .given((List::Regex, "example.com".to_owned()))
            .will_return(Ok(()));

        let service = ListServiceImpl {
            repo: Box::new(repo.clone()),
            env: &env,
            ftl: &ftl
        };

        service.add(List::Regex, "example.com").unwrap();

        assert!(verify(
            repo.add
                .was_called_with((List::Regex, "example.com".to_owned()))
        ));
    }

    #[test]
    fn delete_whitelist() {
        delete_test(List::White, "whitelist.com");
    }

    #[test]
    fn delete_blacklist() {
        delete_test(List::Black, "blacklist.com");
    }

    #[test]
    fn delete_regexlist() {
        delete_test(List::Regex, "regex.com");
    }
}

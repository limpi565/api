// Pi-hole: A black hole for Internet advertisements
// (c) 2019 Pi-hole, LLC (https://pi-hole.net)
// Network-wide ad blocking via your own hardware.
//
// API
// Dnsmasq Configuration Generator
//
// This file is copyright under the latest version of the EUPL.
// Please see LICENSE file for your rights under this license.

use crate::{
    env::{Env, PiholeFile},
    settings::{ConfigEntry, SetupVarsEntry},
    util::{Error, ErrorKind}
};
use failure::ResultExt;
use std::{
    fs::File,
    io::{BufWriter, Write}
};

const DNSMASQ_HEADER: &str = "\
################################################################
#       THIS FILE IS AUTOMATICALLY GENERATED BY PI-HOLE.       #
#          ANY CHANGES MADE TO THIS FILE WILL BE LOST.         #
#                                                              #
#  NEW CONFIG SETTINGS MUST BE MADE IN A SEPARATE CONFIG FILE  #
#                OR IN /etc/dnsmasq.conf                       #
################################################################

localise-queries
local-ttl=2
cache-size=10000
";

/// Generate a dnsmasq config based off of SetupVars.
pub fn generate_dnsmasq_config(env: &Env) -> Result<(), Error> {
    let mut config_file = open_config(env)?;

    write_header(&mut config_file)?;
    write_servers(&mut config_file, env)?;
    write_lists(&mut config_file)?;
    write_dns_options(&mut config_file, env)?;
    write_dhcp(&mut config_file, env)?;

    Ok(())
}

/// Open the dnsmasq config and truncate it
fn open_config(env: &Env) -> Result<BufWriter<File>, Error> {
    env.write_file(PiholeFile::DnsmasqConfig, false)
        .map(BufWriter::new)
}

/// Write the header to the config file
fn write_header(config_file: &mut BufWriter<File>) -> Result<(), Error> {
    config_file
        .write_all(DNSMASQ_HEADER.as_bytes())
        .context(ErrorKind::DnsmasqConfigWrite)
        .map_err(Error::from)
}

/// Write the upstream DNS servers
fn write_servers(config_file: &mut BufWriter<File>, env: &Env) -> Result<(), Error> {
    for i in 1.. {
        let dns = SetupVarsEntry::PiholeDns(i).read(env)?;

        // When the setting is empty, we are finished adding servers
        if dns.is_empty() {
            break;
        }

        writeln!(config_file, "server={}", dns).context(ErrorKind::DnsmasqConfigWrite)?;
    }

    Ok(())
}

/// Write the blocklist, blacklist, and local list
fn write_lists(config_file: &mut BufWriter<File>) -> Result<(), Error> {
    // Always write the blocklist and blacklist, even if Pi-hole is disabled.
    // When Pi-hole is disabled, the files will be empty. This is to make
    // enabling/disabling very fast.
    config_file
        .write_all(b"addn-hosts=/etc/pihole/gravity.list\n")
        .context(ErrorKind::DnsmasqConfigWrite)?;
    config_file
        .write_all(b"addn-hosts=/etc/pihole/black.list\n")
        .context(ErrorKind::DnsmasqConfigWrite)?;

    // Always add local.list after the blocklists
    config_file
        .write_all(b"addn-hosts=/etc/pihole/local.list\n")
        .context(ErrorKind::DnsmasqConfigWrite)?;

    Ok(())
}

/// Write various DNS settings
fn write_dns_options(config_file: &mut BufWriter<File>, env: &Env) -> Result<(), Error> {
    if SetupVarsEntry::QueryLogging.is_true(env)? {
        config_file
            .write_all(
                b"log-queries\n\
            log-facility=/var/log/pihole.log\n\
            log-async\n"
            )
            .context(ErrorKind::DnsmasqConfigWrite)?;
    }

    if SetupVarsEntry::DnsFqdnRequired.is_true(env)? {
        config_file
            .write_all(b"domain-needed\n")
            .context(ErrorKind::DnsmasqConfigWrite)?;
    }

    if SetupVarsEntry::DnsBogusPriv.is_true(env)? {
        config_file
            .write_all(b"bogus-priv\n")
            .context(ErrorKind::DnsmasqConfigWrite)?;
    }

    if SetupVarsEntry::Dnssec.is_true(env)? {
        config_file.write_all(
            b"dnssec\n\
            trust-anchor=.,19036,8,2,49AAC11D7B6F6446702E54A1607371607A1A41855200FD2CE1CDDE32F24E8FB5\n\
            trust-anchor=.,20326,8,2,E06D44B80B8F1D39A95C0B0D7C65D08458E880409BBC683457104237C7F8EC8D\n"
        ).context(ErrorKind::DnsmasqConfigWrite)?;
    }

    let host_record = SetupVarsEntry::HostRecord.read(env)?;
    if !host_record.is_empty() {
        writeln!(config_file, "host-record={}", host_record)
            .context(ErrorKind::DnsmasqConfigWrite)?;
    }

    match SetupVarsEntry::DnsmasqListening.read(env)?.as_str() {
        "all" => config_file
            .write_all(b"except-interface=nonexisting\n")
            .context(ErrorKind::DnsmasqConfigWrite)?,
        "local" => config_file
            .write_all(b"local-service\n")
            .context(ErrorKind::DnsmasqConfigWrite)?,
        "single" | _ => {
            writeln!(
                config_file,
                "interface={}",
                SetupVarsEntry::PiholeInterface.read(env)?
            )
            .context(ErrorKind::DnsmasqConfigWrite)?;
        }
    }

    if SetupVarsEntry::ConditionalForwarding.is_true(env)? {
        let ip = SetupVarsEntry::ConditionalForwardingIp.read(env)?;

        writeln!(
            config_file,
            "server=/{}/{}\nserver=/{}/{}",
            SetupVarsEntry::ConditionalForwardingDomain.read(env)?,
            ip,
            SetupVarsEntry::ConditionalForwardingReverse.read(env)?,
            ip
        )
        .context(ErrorKind::DnsmasqConfigWrite)?;
    }

    Ok(())
}

/// Write DHCP settings, if enabled
fn write_dhcp(config_file: &mut BufWriter<File>, env: &Env) -> Result<(), Error> {
    if !SetupVarsEntry::DhcpActive.is_true(env)? {
        // Skip DHCP settings if it is not enabled
        return Ok(());
    }

    let lease_time: usize = SetupVarsEntry::DhcpLeasetime.read_as(env)?;
    let lease_time = if lease_time == 0 {
        "infinite".to_owned()
    } else {
        format!("{}h", lease_time)
    };

    // Main DHCP settings. The "wpad" lines fix CERT vulnerability VU#598349 by
    // preventing clients from using "wpad" as their hostname.
    writeln!(
        config_file,
        "dhcp-authoritative\n\
         dhcp-leasefile=/etc/pihole/dhcp.leases\n\
         dhcp-range={},{},{}\n\
         dhcp-option=option:router,{}\n\
         dhcp-name-match=set:wpad-ignore,wpad\n\
         dhcp-ignore-names=tag:wpad-ignore",
        SetupVarsEntry::DhcpStart.read(env)?,
        SetupVarsEntry::DhcpEnd.read(env)?,
        lease_time,
        SetupVarsEntry::DhcpRouter.read(env)?
    )
    .context(ErrorKind::DnsmasqConfigWrite)?;

    // Additional settings for IPv6
    if SetupVarsEntry::DhcpIpv6.is_true(env)? {
        writeln!(
            config_file,
            "dhcp-option=option6:dns-server,[::]\n\
             dhcp-range=::100,::1ff,constructor:{},ra-names,slaac,{}\n\
             ra-param=*,0,0",
            SetupVarsEntry::PiholeInterface.read(env)?,
            lease_time
        )
        .context(ErrorKind::DnsmasqConfigWrite)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        open_config, write_dhcp, write_dns_options, write_header, write_lists, write_servers,
        DNSMASQ_HEADER
    };
    use crate::{
        env::{Env, PiholeFile},
        testing::TestEnvBuilder,
        util::Error
    };
    use std::{
        fs::File,
        io::{BufWriter, Write}
    };

    /// Generalized test for dnsmasq config generation. This sets up SetupVars
    /// with the initial data, runs `test_fn`, then verifies that the
    /// dnsmasq config content matches the expected content.
    ///
    /// # Arguments
    /// - `expected_config`: The expected contents of the dnsmasq config after
    /// running `test_fn`. The dnsmasq config starts out empty.
    /// - `setup_vars`: The initial contents of SetupVars
    /// - `test_fn`: The function to run for the test. It takes in the buffered
    /// file writer and the environment data.
    fn test_config(
        expected_config: &str,
        setup_vars: &str,
        test_fn: impl Fn(&mut BufWriter<File>, &Env) -> Result<(), Error>
    ) {
        let env_builder = TestEnvBuilder::new()
            .file_expect(PiholeFile::DnsmasqConfig, "", expected_config)
            .file(PiholeFile::SetupVars, setup_vars);

        let mut dnsmasq_config = env_builder.clone_test_files().into_iter().next().unwrap();
        let env = env_builder.build();
        let mut file_writer = open_config(&env).unwrap();

        test_fn(&mut file_writer, &env).unwrap();
        file_writer.flush().unwrap();

        let mut buffer = String::new();
        dnsmasq_config.assert_expected(&mut buffer);
    }

    /// Confirm that the header is written
    #[test]
    fn header_written() {
        test_config(DNSMASQ_HEADER, "", |writer, _env| write_header(writer));
    }

    /// Confirm all (sequential) DNS servers listed are written
    #[test]
    fn dns_servers_all_written() {
        test_config(
            "server=8.8.8.8\nserver=8.8.4.4\n",
            "PIHOLE_DNS_1=8.8.8.8\n\
             PIHOLE_DNS_2=8.8.4.4",
            write_servers
        );
    }

    /// Confirm that non-sequential DNS servers are ignored, that is, stop at
    /// the first empty server
    #[test]
    fn ignore_non_sequential_dns_servers() {
        test_config(
            "server=8.8.8.8\nserver=8.8.4.4\n",
            "PIHOLE_DNS_1=8.8.8.8\n\
             PIHOLE_DNS_2=8.8.4.4\n\
             PIHOLE_DNS_4=1.1.1.1",
            write_servers
        );
    }

    /// Confirm that the blocklists are written (in addition to local.list)
    #[test]
    fn block_lists_written() {
        test_config(
            "addn-hosts=/etc/pihole/gravity.list\n\
             addn-hosts=/etc/pihole/black.list\n\
             addn-hosts=/etc/pihole/local.list\n",
            "",
            |config, _| write_lists(config)
        );
    }

    /// Generate the DNS options configuration when there are minimal settings
    /// enabled
    #[test]
    fn minimal_dns_options() {
        test_config(
            "interface=eth0\n",
            "DNS_FQDN_REQUIRED=false\n\
             DNS_BOGUS_PRIV=false\n\
             DNSSEC=false\n\
             HOSTRECORD=\n\
             DNSMASQ_LISTENING=single\n\
             PIHOLE_INTERFACE=eth0\n\
             CONDITIONAL_FORWARDING=false",
            write_dns_options
        );
    }

    /// Generate the DNS options configuration with all the settings enabled.
    #[test]
    fn maximal_dns_options() {
        test_config(
            "domain-needed\n\
            bogus-priv\n\
            dnssec\n\
            trust-anchor=.,19036,8,2,49AAC11D7B6F6446702E54A1607371607A1A41855200FD2CE1CDDE32F24E8FB5\n\
            trust-anchor=.,20326,8,2,E06D44B80B8F1D39A95C0B0D7C65D08458E880409BBC683457104237C7F8EC8D\n\
            host-record=domain.com,127.0.0.1\n\
            local-service\n\
            server=/domain.com/8.8.8.8\n\
            server=/8.8.8.in-addr.arpa/8.8.8.8\n",
            "DNS_FQDN_REQUIRED=true\n\
            DNS_BOGUS_PRIV=true\n\
            DNSSEC=true\n\
            HOSTRECORD=domain.com,127.0.0.1\n\
            DNSMASQ_LISTENING=local\n\
            CONDITIONAL_FORWARDING=true\n\
            CONDITIONAL_FORWARDING_IP=8.8.8.8\n\
            CONDITIONAL_FORWARDING_DOMAIN=domain.com\n\
            CONDITIONAL_FORWARDING_REVERSE=8.8.8.in-addr.arpa",
            write_dns_options
        );
    }

    /// No DHCP settings should be written if DHCP is inactive
    #[test]
    fn dhcp_inactive() {
        test_config(
            "",
            "PIHOLE_INTERFACE=eth0\n\
             DHCP_ACTIVE=false\n\
             DHCP_START=192.168.1.50\n\
             DHCP_END=192.168.1.150\n\
             DHCP_ROUTER=192.168.1.1\n\
             DHCP_LEASETIME=24\n\
             PIHOLE_DOMAIN=lan\n\
             DHCP_IPv6=false",
            write_dhcp
        )
    }

    /// DHCP settings should be written if DHCP is active. IPv6 is not enabled
    /// and those settings should not appear.
    #[test]
    fn dhcp_active() {
        test_config(
            "dhcp-authoritative\n\
             dhcp-leasefile=/etc/pihole/dhcp.leases\n\
             dhcp-range=192.168.1.50,192.168.1.150,24h\n\
             dhcp-option=option:router,192.168.1.1\n\
             dhcp-name-match=set:wpad-ignore,wpad\n\
             dhcp-ignore-names=tag:wpad-ignore\n",
            "PIHOLE_INTERFACE=eth0\n\
             DHCP_ACTIVE=true\n\
             DHCP_START=192.168.1.50\n\
             DHCP_END=192.168.1.150\n\
             DHCP_ROUTER=192.168.1.1\n\
             DHCP_LEASETIME=24\n\
             PIHOLE_DOMAIN=lan\n\
             DHCP_IPv6=false",
            write_dhcp
        )
    }

    /// DHCP IPv6 settings are written if IPv6 is enabled
    #[test]
    fn dhcp_ipv6() {
        test_config(
            "dhcp-authoritative\n\
             dhcp-leasefile=/etc/pihole/dhcp.leases\n\
             dhcp-range=192.168.1.50,192.168.1.150,24h\n\
             dhcp-option=option:router,192.168.1.1\n\
             dhcp-name-match=set:wpad-ignore,wpad\n\
             dhcp-ignore-names=tag:wpad-ignore\n\
             dhcp-option=option6:dns-server,[::]\n\
             dhcp-range=::100,::1ff,constructor:eth0,ra-names,slaac,24h\n\
             ra-param=*,0,0\n",
            "PIHOLE_INTERFACE=eth0\n\
             DHCP_ACTIVE=true\n\
             DHCP_START=192.168.1.50\n\
             DHCP_END=192.168.1.150\n\
             DHCP_ROUTER=192.168.1.1\n\
             DHCP_LEASETIME=24\n\
             PIHOLE_DOMAIN=lan\n\
             DHCP_IPv6=true",
            write_dhcp
        )
    }

    /// An infinite lease (`DHCP_LEASETIME=0`) is written as "infinite" in the
    /// settings. This test also checks the IPv6 settings.
    #[test]
    fn dhcp_infinite_lease() {
        test_config(
            "dhcp-authoritative\n\
             dhcp-leasefile=/etc/pihole/dhcp.leases\n\
             dhcp-range=192.168.1.50,192.168.1.150,infinite\n\
             dhcp-option=option:router,192.168.1.1\n\
             dhcp-name-match=set:wpad-ignore,wpad\n\
             dhcp-ignore-names=tag:wpad-ignore\n\
             dhcp-option=option6:dns-server,[::]\n\
             dhcp-range=::100,::1ff,constructor:eth0,ra-names,slaac,infinite\n\
             ra-param=*,0,0\n",
            "PIHOLE_INTERFACE=eth0\n\
             DHCP_ACTIVE=true\n\
             DHCP_START=192.168.1.50\n\
             DHCP_END=192.168.1.150\n\
             DHCP_ROUTER=192.168.1.1\n\
             DHCP_LEASETIME=0\n\
             PIHOLE_DOMAIN=lan\n\
             DHCP_IPv6=true",
            write_dhcp
        )
    }
}

extern crate std;

use std::io;
use std::io::{Read, Write};
use std::ffi::CString;

use consts::*;
use u2ftypes::*;
use util::io_err;

////////////////////////////////////////////////////////////////////////
// Device Commands
////////////////////////////////////////////////////////////////////////

pub fn init_device<T>(dev: &mut T, nonce: [u8; 8]) -> io::Result<()>
where
    T: U2FDevice + Read + Write,
{
    let raw = sendrecv(dev, U2FHID_INIT, &nonce)?;
    dev.set_cid(U2FHIDInitResp::read(&raw, &nonce)?);
    Ok(())
}

pub fn ping_device<T>(dev: &mut T, random: [u8; 8]) -> io::Result<()>
where
    T: U2FDevice + Read + Write,
{
    if sendrecv(dev, U2FHID_PING, &random)? != random {
        return Err(io_err("Ping was corrupted!"));
    }

    Ok(())
}

fn status_word_to_error(status_word_high: u8, status_word_low: u8) -> Option<io::Error> {
    let status_word = [status_word_high, status_word_low];

    match status_word {
        SW_NO_ERROR => None,
        SW_WRONG_LENGTH => Some(io::Error::new(io::ErrorKind::InvalidInput, "Wrong Length")),
        SW_WRONG_DATA => Some(io::Error::new(io::ErrorKind::InvalidData, "Wrong Data")),
        SW_CONDITIONS_NOT_SATISFIED => Some(io::Error::new(
            io::ErrorKind::TimedOut,
            "Conditions not satisfied",
        )),
        _ => {
            Some(io::Error::new(
                io::ErrorKind::Other,
                format!("Problem Status: {:?}", status_word),
            ))
        }
    }
}

pub fn u2f_version<T>(dev: &mut T) -> io::Result<std::ffi::CString>
where
    T: U2FDevice + Read + Write,
{
    let mut version_resp = send_apdu(dev, U2F_VERSION, 0x00, &vec![])?;
    let sw_low = version_resp.pop().unwrap_or_default();
    let sw_high = version_resp.pop().unwrap_or_default();

    match status_word_to_error(sw_high, sw_low) {
        None => Ok(CString::new(version_resp)?),
        Some(e) => Err(e),
    }
}

pub fn u2f_version_is_v2<T>(dev: &mut T) -> io::Result<()>
where
    T: U2FDevice + Read + Write,
{
    let version_string = u2f_version(dev)?;

    if version_string != CString::new("U2F_V2")? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unexpected U2F Version",
        ));
    }
    Ok(())
}

pub fn u2f_register<T>(
    dev: &mut T,
    challenge: &Vec<u8>,
    application: &Vec<u8>,
) -> io::Result<Vec<u8>>
where
    T: U2FDevice + Read + Write,
{
    if challenge.len() != PARAMETER_SIZE || application.len() != PARAMETER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid parameter sizes",
        ));
    }

    let flags = 0x00;

    let mut register_data = Vec::with_capacity(2 * PARAMETER_SIZE);
    register_data.extend(challenge);
    register_data.extend(application);

    let register_resp = send_apdu(
        dev,
        U2F_REGISTER,
        flags | U2F_REQUEST_USER_PRESENCE,
        &register_data,
    )?;

    if register_resp.len() != 2 {
        // Real data, we're done
        return Ok(register_resp);
    }

    match status_word_to_error(register_resp[0], register_resp[1]) {
        None => Ok(Vec::new()),
        Some(e) => Err(e),
    }
}

pub fn u2f_sign<T>(
    dev: &mut T,
    challenge: &Vec<u8>,
    application: &Vec<u8>,
    key_handle: &Vec<u8>,
) -> io::Result<Vec<u8>>
where
    T: U2FDevice + Read + Write,
{
    if challenge.len() != PARAMETER_SIZE || application.len() != PARAMETER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid parameter sizes",
        ));
    }

    if key_handle.len() > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Key handle too large",
        ));
    }

    let mut sign_data = Vec::with_capacity(2 * PARAMETER_SIZE + 1 + key_handle.len());
    sign_data.extend(challenge);
    sign_data.extend(application);
    sign_data.push(key_handle.len() as u8);
    sign_data.extend(key_handle);

    let flags = U2F_REQUEST_USER_PRESENCE;
    let sign_resp = send_apdu(dev, U2F_AUTHENTICATE, flags, &sign_data)?;

    if sign_resp.len() != 2 {
        // Real data, let's bail out here
        return Ok(sign_resp);
    }

    match status_word_to_error(sign_resp[0], sign_resp[1]) {
        None => Ok(Vec::new()),
        Some(e) => Err(e),
    }
}

pub fn u2f_is_keyhandle_valid<T>(
    dev: &mut T,
    challenge: &Vec<u8>,
    application: &Vec<u8>,
    key_handle: &Vec<u8>,
) -> io::Result<bool>
where
    T: U2FDevice + Read + Write,
{
    if challenge.len() != PARAMETER_SIZE || application.len() != PARAMETER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid parameter sizes",
        ));
    }

    if key_handle.len() > 256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Key handle too large",
        ));
    }

    let mut sign_data = Vec::with_capacity(2 * PARAMETER_SIZE + 1 + key_handle.len());
    sign_data.extend(challenge);
    sign_data.extend(application);
    sign_data.push(key_handle.len() as u8);
    sign_data.extend(key_handle);

    let flags = U2F_CHECK_IS_REGISTERED;
    let sign_resp = send_apdu(dev, U2F_AUTHENTICATE, flags, &sign_data)?;
    Ok(sign_resp == SW_CONDITIONS_NOT_SATISFIED)
}

////////////////////////////////////////////////////////////////////////
// Device Communication Functions
////////////////////////////////////////////////////////////////////////

pub fn sendrecv<T>(dev: &mut T, cmd: u8, send: &[u8]) -> io::Result<Vec<u8>>
where
    T: U2FDevice + Read + Write,
{
    // Send initialization packet.
    let mut count = U2FHIDInit::write(dev, cmd, send)?;

    // Send continuation packets.
    let mut sequence = 0u8;
    while count < send.len() {
        count += U2FHIDCont::write(dev, sequence, &send[count..])?;
        sequence += 1;
    }

    // Now we read. This happens in 2 chunks: The initial packet, which has the
    // size we expect overall, then continuation packets, which will fill in
    // data until we have everything.
    let mut data = U2FHIDInit::read(dev)?;

    let mut sequence = 0u8;
    while data.len() < data.capacity() {
        let max = data.capacity() - data.len();
        data.extend_from_slice(&U2FHIDCont::read(dev, sequence, max)?);
        sequence += 1;
    }

    Ok(data)
}

fn send_apdu<T>(dev: &mut T, cmd: u8, p1: u8, send: &Vec<u8>) -> io::Result<Vec<u8>>
where
    T: U2FDevice + Read + Write,
{
    sendrecv(dev, U2FHID_MSG, &U2FAPDUHeader::to_bytes(cmd, p1, send)?)
}

#[cfg(test)]
mod tests {
    use super::{U2FDevice, init_device, ping_device, sendrecv, send_apdu};
    use std::error::Error;
    use consts::{U2FHID_PING, U2FHID_MSG};
    mod platform {
        use consts::{CID_BROADCAST, HID_RPT_SIZE};
        use u2ftypes::U2FDevice;
        use std::io;
        use std::io::{Read, Write};

        pub struct TestDevice {
            pub cid: [u8; 4],
            pub expected_reads: Vec<[u8; HID_RPT_SIZE]>,
            pub expected_writes: Vec<[u8; HID_RPT_SIZE + 1]>,
        }

        impl TestDevice {
            pub fn new() -> TestDevice {
                TestDevice {
                    cid: CID_BROADCAST,
                    expected_reads: Vec::new(),
                    expected_writes: Vec::new(),
                }
            }
            pub fn add_write(&mut self, packet: &[u8], fill_value: u8) {
                // Add one to deal with record index check
                let mut write: [u8; HID_RPT_SIZE + 1] = [fill_value; HID_RPT_SIZE + 1];
                // Make sure we start with a 0, for HID record index
                write[0] = 0;
                // Clone packet data in at 1, since front is padded with HID record index
                write[1..packet.len() + 1].clone_from_slice(&packet);
                self.expected_writes.push(write);
            }
            pub fn add_read(&mut self, packet: &[u8], fill_value: u8) {
                let mut read: [u8; HID_RPT_SIZE] = [fill_value; HID_RPT_SIZE];
                read[0..packet.len()].clone_from_slice(&packet);
                self.expected_reads.push(read);
            }
        }

        impl Write for TestDevice {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                // Pop a vector from the expected writes, check for quality
                // against bytes array.
                assert!(
                    self.expected_writes.len() > 0,
                    "Ran out of expected write values!"
                );
                let check = self.expected_writes.remove(0);
                assert_eq!(check.len(), bytes.len());
                assert_eq!(&check[..], bytes);
                Ok(bytes.len())
            }
            // nop
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Read for TestDevice {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                // Pop a vector from the expected writes, check for quality
                // against bytes array.
                assert!(
                    self.expected_reads.len() > 0,
                    "Ran out of expected read values!"
                );
                let check = self.expected_reads.remove(0);
                bytes.clone_from_slice(&check[..]);
                Ok(check.len())
            }
        }
        impl U2FDevice for TestDevice {
            fn get_cid<'a>(&'a self) -> &'a [u8; 4] {
                &self.cid
            }
            fn set_cid(&mut self, cid: [u8; 4]) {
                self.cid = cid;
            }
        }
    }

    #[test]
    fn test_init_device() {
        let mut device = platform::TestDevice::new();
        let nonce = [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];

        device.add_write(
            &vec![
                0xff,
                0xff,
                0xff,
                0xff,
                0x86,
                0x00,
                0x08,
                0x08,
                0x07,
                0x06,
                0x05,
                0x04,
                0x03,
                0x02,
                0x01,
            ],
            0,
        );
        device.add_read(
            &vec![
                0xff,
                0xff,
                0xff,
                0xff,
                0x86,
                0x00,
                0x11,
                0x08,
                0x07,
                0x06,
                0x05,
                0x04,
                0x03,
                0x02,
                0x01,
                0x00,
                0x03,
                0x00,
                0x14,
                0x02,
                0x04,
                0x01,
                0x08,
                0x01,
            ],
            0,
        );
        if let Err(e) = init_device(&mut device, nonce) {
            assert!(
                true,
                format!("Init device returned an error! {:?}", e.description())
            );
        }
        assert_eq!(device.get_cid(), &[0x00, 0x03, 0x00, 0x14]);
    }

    #[test]
    fn test_sendrecv_multiple() {
        let mut device = platform::TestDevice::new();
        device.set_cid([1, 2, 3, 4]);
        device.add_write(&vec![0x01, 0x02, 0x03, 0x04, U2FHID_PING, 0x00, 0xe4], 1);
        // Need CID and sequence number for CONT packets
        device.add_write(&vec![0x01, 0x02, 0x03, 0x04, 0x00], 1);
        device.add_write(&vec![0x01, 0x02, 0x03, 0x04, 0x01], 1);
        device.add_write(
            &vec![
                0x01,
                0x02,
                0x03,
                0x04,
                0x02,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
            ],
            0,
        );
        device.add_read(&vec![0x01, 0x02, 0x03, 0x04, U2FHID_PING, 0x00, 0xe4], 1);
        // Need CID and sequence number for CONT packets
        device.add_read(&vec![0x01, 0x02, 0x03, 0x04, 0x00], 1);
        device.add_read(&vec![0x01, 0x02, 0x03, 0x04, 0x01], 1);
        device.add_read(
            &vec![
                0x01,
                0x02,
                0x03,
                0x04,
                0x02,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
                0x01,
            ],
            0,
        );

        let d = match sendrecv(&mut device, U2FHID_PING, &vec![1 as u8; 0xe4]) {
            Ok(c) => c,
            Err(e) => {
                panic!(format!(
                    "Init device returned an error! {:?}",
                    e.description()
                ))
            }
        };
        assert_eq!(d.len(), 0xe4);
        assert_eq!(d, vec![1 as u8; 0xe4]);
    }

    #[test]
    fn test_sendapdu() {
        let mut device = platform::TestDevice::new();
        device.set_cid([1, 2, 3, 4]);
        device.add_write(
            &vec![
                // sendrecv header
                0x01,
                0x02,
                0x03,
                0x04,
                U2FHID_MSG,
                0x00,
                0x0e,
                // apdu header
                0x00,
                U2FHID_PING,
                0xaa,
                0x00,
                0x00,
                0x00,
                0x05,
                // apdu data
                0x01,
                0x02,
                0x03,
                0x04,
                0x05,
            ],
            0,
        );
        // Only expect data from APDU back
        device.add_read(
            &vec![
                0x01,
                0x02,
                0x03,
                0x04,
                U2FHID_MSG,
                0x00,
                0x05,
                0x01,
                0x02,
                0x03,
                0x04,
                0x05,
            ],
            0,
        );
        assert!(send_apdu(&mut device, U2FHID_PING, 0xaa, &vec![1, 2, 3, 4, 5]).is_ok());
    }

    #[test]
    fn test_ping_device() {
        let mut device = platform::TestDevice::new();
        device.set_cid([1, 2, 3, 4]);
        device.add_write(
            &vec![
                // apdu header
                0x01,
                0x02,
                0x03,
                0x04,
                U2FHID_PING,
                0x00,
                0x08,
                // ping nonce
                0x01,
                0x02,
                0x03,
                0x04,
                0x05,
                0x06,
                0x07,
                0x08,
            ],
            0,
        );
        // Only expect data from APDU back
        device.add_read(
            &vec![
                0x01,
                0x02,
                0x03,
                0x04,
                U2FHID_MSG,
                0x00,
                0x08,
                0x01,
                0x02,
                0x03,
                0x04,
                0x05,
                0x06,
                0x07,
                0x08,
            ],
            0,
        );

        let random = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];

        if let Err(e) = ping_device(&mut device, random) {
            assert!(
                true,
                format!("Init device returned an error! {:?}", e.description())
            );
        }
    }
}

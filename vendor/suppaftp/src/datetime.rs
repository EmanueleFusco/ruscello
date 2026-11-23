use jiff::civil::DateTime;

use crate::{
    regex::MDTM_RE,
    types::{FtpError, FtpResult},
};

pub(crate) fn parse_mdtm_response(body: &str) -> FtpResult<DateTime> {
    let value = MDTM_RE.find(body).ok_or(FtpError::BadResponse)?;
    // Keep MDTM validation strict before Jiff normalizes a leap second to 59.
    if value.as_str().get(12..14) == Some("60") {
        return Err(FtpError::BadResponse);
    }
    DateTime::strptime("%Y%m%d%H%M%S", value.as_str()).map_err(|_| FtpError::BadResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_seconds_used_as_ftp_resume_validators() -> FtpResult<()> {
        for body in ["213 20240229030506\r\n", "213 20240229030506.031\r\n"] {
            let date = parse_mdtm_response(body)?;
            assert_eq!(date.strftime("%Y%m%d%H%M%S").to_string(), "20240229030506");
        }
        assert!(parse_mdtm_response("213 20230229030506").is_err());
        assert!(parse_mdtm_response("213 20240229240506").is_err());
        assert!(parse_mdtm_response("213 20240229030560").is_err());
        Ok(())
    }
}

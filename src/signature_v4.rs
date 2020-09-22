//! AWS Signature Version 4
//!
//! See <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>
//!

use crate::headers::AmzDate;
use crate::utils::{crypto, Also, Apply, OrderedHeaders};

use bytes::Bytes;
use hyper::Method;
use smallvec::SmallVec;

/// custom uri encode
fn uri_encode(output: &mut String, input: &str, encode_slash: bool) {
    /// hex uppercase table
    const HEX_UPPERCASE_TABLE: [u8; 16] = *b"0123456789ABCDEF";

    let mut buf: SmallVec<[u8; 512]> = SmallVec::with_capacity(input.len());

    for &byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'~' | b'.' => buf.push(byte),
            b'/' => {
                if encode_slash {
                    buf.push(b'%');
                    buf.push(b'2');
                    buf.push(b'F');
                } else {
                    buf.push(byte);
                }
            }
            _ => {
                macro_rules! to_hex {
                    ($n:expr) => {{
                        #[allow(clippy::unreachable)]
                        *HEX_UPPERCASE_TABLE
                            .get(usize::from($n))
                            .unwrap_or_else(|| unreachable!()) // a 4-bits number is always less then 16
                    }};
                }

                buf.push(b'%');
                buf.push(to_hex!(byte.wrapping_shr(4)));
                buf.push(to_hex!(byte & 15));
            }
        }
    }

    #[allow(clippy::unreachable)]
    std::str::from_utf8(buf.as_ref())
        .unwrap_or_else(|_| unreachable!()) // an ascii string is always a utf-8 string
        .apply(|s| output.push_str(s))
}

/// is skipped header
fn is_skipped_header(header: &str) -> bool {
    ["authorization", "user-agent"].contains(&header)
}

/// sha256 hash of an empty string
const EMPTY_STRING_SHA256_HASH: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Payload
pub enum Payload<'a> {
    /// empty
    Empty,
    /// single chunk
    SingleChunk(&'a [u8]),
    /// multiple chunks
    #[allow(dead_code)] // TODO: remove this allow
    MultipleChunks,
}

/// create canonical request
pub fn create_canonical_request(
    method: &Method,
    uri_path: &str,
    query_strings: &[(String, String)],
    headers: &OrderedHeaders<'_>,
    payload: Payload<'_>,
) -> String {
    String::with_capacity(256)
        .also(|ans| {
            // <HTTPMethod>\n
            ans.push_str(method.as_str());
            ans.push('\n');
        })
        .also(|ans| {
            // <CanonicalURI>\n
            uri_encode(ans, uri_path, false);
            ans.push('\n');
        })
        .also(|ans| {
            // <CanonicalQueryString>\n
            let encoded_query_strings: SmallVec<[(String, String); 16]> = query_strings
                .iter()
                .map(|(n, v)| {
                    let name = String::with_capacity(n.len()).also(|s| uri_encode(s, n, true));
                    let value = String::with_capacity(v.len()).also(|s| uri_encode(s, v, true));
                    (name, value)
                })
                .collect::<SmallVec<[(String, String); 16]>>()
                .also(|qs| qs.sort());

            if let Some((first, remain)) = encoded_query_strings.split_first() {
                {
                    let (name, value) = first;
                    ans.push_str(name);
                    ans.push('=');
                    ans.push_str(value);
                }
                for (name, value) in remain {
                    ans.push('&');
                    ans.push_str(name);
                    ans.push('=');
                    ans.push_str(value);
                }
            }

            ans.push('\n');
        })
        .also(|ans| {
            // <CanonicalHeaders>\n

            // FIXME: check HOST, Content-Type, x-amz-security-token, x-amz-content-sha256

            for &(name, value) in headers.as_ref().iter() {
                if is_skipped_header(name) {
                    continue;
                }
                ans.push_str(name);
                ans.push(':');
                ans.push_str(value.trim());
                ans.push('\n');
            }
            ans.push('\n');
        })
        .also(|ans| {
            // <SignedHeaders>\n
            let mut first_flag = true;
            for &(name, _) in headers.as_ref().iter() {
                if is_skipped_header(name) {
                    continue;
                }
                if first_flag {
                    first_flag = false;
                } else {
                    ans.push(';');
                }
                ans.push_str(name);
            }

            ans.push('\n');
        })
        .also(|ans| {
            // <HashedPayload>
            match payload {
                Payload::Empty => ans.push_str(EMPTY_STRING_SHA256_HASH),
                Payload::SingleChunk(data) => ans.push_str(&crypto::hex_sha256(data)),
                Payload::MultipleChunks => ans.push_str("STREAMING-AWS4-HMAC-SHA256-PAYLOAD"),
            }
            drop(payload)
        })
}

/// create string to sign
pub fn create_string_to_sign(canonical_request: &str, amz_date: &AmzDate, region: &str) -> String {
    String::with_capacity(256)
        .also(|ans| {
            // <Algorithm>\n
            ans.push_str("AWS4-HMAC-SHA256\n");
        })
        .also(|ans| {
            // <RequestDateTime>\n
            ans.push_str(&amz_date.to_iso8601());
            ans.push('\n');
        })
        .also(|ans| {
            // <CredentialScope>\n
            ans.push_str(&amz_date.to_date());
            ans.push('/');
            ans.push_str(region); // TODO: use a `Region` type
            ans.push_str("/s3/aws4_request\n");
        })
        .also(|ans| {
            // <HashedCanonicalRequest>
            ans.push_str(&crypto::hex_sha256(canonical_request.as_bytes()))
        })
}

/// create `string_to_sign` of a chunk
pub fn create_chunk_string_to_sign(
    amz_date: &AmzDate,
    region: &str,
    prev_signature: &str,
    chunk_data: &[Bytes],
) -> String {
    String::with_capacity(256)
        .also(|ans| {
            ans.push_str("AWS4-HMAC-SHA256-PAYLOAD\n");
        })
        .also(|ans| {
            ans.push_str(&amz_date.to_iso8601());
            ans.push('\n');
        })
        .also(|ans| {
            ans.push_str(&amz_date.to_date());
            ans.push('/');
            ans.push_str(region); // TODO: use a `Region` type
            ans.push_str("/s3/aws4_request\n");
        })
        .also(|ans| {
            ans.push_str(prev_signature);
            ans.push('\n');
        })
        .also(|ans| {
            ans.push_str(EMPTY_STRING_SHA256_HASH);
            ans.push('\n');
        })
        .also(|ans| {
            if chunk_data.is_empty() {
                ans.push_str(EMPTY_STRING_SHA256_HASH);
            } else {
                ans.push_str(&crypto::hex_sha256_chunk(chunk_data));
            }
        })
}

/// calculate signature
pub fn calculate_signature(
    string_to_sign: &str,
    secret_key: &str,
    amz_date: &AmzDate,
    region: &str,
) -> String {
    let secret = <SmallVec<[u8; 128]>>::with_capacity(secret_key.len().saturating_add(4))
        .also(|v| v.extend_from_slice(b"AWS4"))
        .also(|v| v.extend_from_slice(secret_key.as_bytes()));

    let date = amz_date.to_date();

    // DateKey
    let date_key = crypto::hmac_sha256(secret.as_ref(), date.as_ref());

    // DateRegionKey
    let date_region_key = crypto::hmac_sha256(date_key.as_ref(), region.as_ref()); // TODO: use a `Region` type

    // DateRegionServiceKey
    let date_region_service_key = crypto::hmac_sha256(date_region_key.as_ref(), "s3".as_ref());

    // SigningKey
    let signing_key =
        crypto::hmac_sha256(date_region_service_key.as_ref(), "aws4_request".as_ref());

    // Signature
    crypto::hex_hmac_sha256(signing_key.as_ref(), string_to_sign.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_get_object() {
        // let access_key_id = "AKIAIOSFODNN7EXAMPLE";
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let timestamp = "20130524T000000Z";
        // let bucket = "examplebucket";
        let region = "us-east-1";
        let path = "/test.txt";

        let headers = OrderedHeaders::from_slice_unchecked(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            (
                "x-amz-content-sha256",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            ("x-amz-date", "20130524T000000Z"),
        ]);

        let method = Method::GET;

        let canonical_request =
            create_canonical_request(&method, path, &[], &headers, Payload::Empty);

        assert_eq!(
            canonical_request,
            concat!(
                "GET\n",
                "/test.txt\n",
                "\n",
                "host:examplebucket.s3.amazonaws.com\n",
                "range:bytes=0-9\n",
                "x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "x-amz-date:20130524T000000Z\n",
                "\n",
                "host;range;x-amz-content-sha256;x-amz-date\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )
        );

        let date = AmzDate::from_header_str(timestamp).unwrap();
        let string_to_sign = create_string_to_sign(&canonical_request, &date, region);
        assert_eq!(
            string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972",
            )
        );

        let signature = calculate_signature(&string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            signature,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn example_put_object_single_chunk() {
        // let access_key_id = "AKIAIOSFODNN7EXAMPLE";
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let timestamp = "20130524T000000Z";
        // let bucket = "examplebucket";
        let region = "us-east-1";
        let path = "/test$file.text";

        let headers = OrderedHeaders::from_slice_unchecked(&[
            ("date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("host", "examplebucket.s3.amazonaws.com"),
            (
                "x-amz-content-sha256",
                "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072",
            ),
            ("x-amz-date", "20130524T000000Z"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ]);

        let method = Method::PUT;
        let payload = "Welcome to Amazon S3.";

        let canonical_request = create_canonical_request(
            &method,
            path,
            &[],
            &headers,
            Payload::SingleChunk(payload.as_bytes()),
        );

        assert_eq!(
            canonical_request,
            concat!(
                "PUT\n",
                "/test%24file.text\n",
                "\n",
                "date:Fri, 24 May 2013 00:00:00 GMT\n",
                "host:examplebucket.s3.amazonaws.com\n",
                "x-amz-content-sha256:44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072\n",
                "x-amz-date:20130524T000000Z\n",
                "x-amz-storage-class:REDUCED_REDUNDANCY\n",
                "\n",
                "date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class\n",
                "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072",
            )
        );

        let date = AmzDate::from_header_str(timestamp).unwrap();
        let string_to_sign = create_string_to_sign(&canonical_request, &date, region);
        assert_eq!(
            string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "9e0e90d9c76de8fa5b200d8c849cd5b8dc7a3be3951ddb7f6a76b4158342019d",
            )
        );

        let signature = calculate_signature(&string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            signature,
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
        );
    }

    #[test]
    fn example_put_object_multiple_chunks_seed_signature() {
        // let access_key_id = "AKIAIOSFODNN7EXAMPLE";
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let timestamp = "20130524T000000Z";
        // let bucket = "examplebucket";
        let region = "us-east-1";
        let path = "/examplebucket/chunkObject.txt";

        let headers = OrderedHeaders::from_slice_unchecked(&[
            ("content-encoding", "aws-chunked"),
            ("content-length", "66824"),
            ("host", "s3.amazonaws.com"),
            ("x-amz-content-sha256", "STREAMING-AWS4-HMAC-SHA256-PAYLOAD"),
            ("x-amz-date", "20130524T000000Z"),
            ("x-amz-decoded-content-length", "66560"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ]);

        let method = Method::PUT;

        let canonical_request =
            create_canonical_request(&method, path, &[], &headers, Payload::MultipleChunks);

        assert_eq!(
            canonical_request,
            concat!(
                "PUT\n",
                "/examplebucket/chunkObject.txt\n",
                "\n",
                "content-encoding:aws-chunked\n",
                "content-length:66824\n",
                "host:s3.amazonaws.com\n",
                "x-amz-content-sha256:STREAMING-AWS4-HMAC-SHA256-PAYLOAD\n",
                "x-amz-date:20130524T000000Z\n",
                "x-amz-decoded-content-length:66560\n",
                "x-amz-storage-class:REDUCED_REDUNDANCY\n",
                "\n",
                "content-encoding;content-length;host;x-amz-content-sha256;x-amz-date;x-amz-decoded-content-length;x-amz-storage-class\n",
                "STREAMING-AWS4-HMAC-SHA256-PAYLOAD",
            )
        );

        let date = AmzDate::from_header_str(timestamp).unwrap();
        let string_to_sign = create_string_to_sign(&canonical_request, &date, region);
        assert_eq!(
            string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "cee3fed04b70f867d036f722359b0b1f2f0e5dc0efadbc082b76c4c60e316455",
            )
        );

        let signature = calculate_signature(&string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            signature,
            "4f232c4386841ef735655705268965c44a0e4690baa4adea153f7db9fa80a0a9",
        );
    }

    #[test]
    fn example_put_object_multiple_chunks_chunk_signature() {
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let timestamp = "20130524T000000Z";
        let region = "us-east-1";
        let date = AmzDate::from_header_str(timestamp).unwrap();

        let seed_signature = "4f232c4386841ef735655705268965c44a0e4690baa4adea153f7db9fa80a0a9";

        let chunk1_string_to_sign = create_chunk_string_to_sign(
            &date,
            region,
            seed_signature,
            &[Bytes::from(vec![b'a'; 64 * 1024])],
        );
        assert_eq!(
            chunk1_string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256-PAYLOAD\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "4f232c4386841ef735655705268965c44a0e4690baa4adea153f7db9fa80a0a9\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "bf718b6f653bebc184e1479f1935b8da974d701b893afcf49e701f3e2f9f9c5a",
            )
        );

        let chunk1_signature =
            calculate_signature(&chunk1_string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            chunk1_signature,
            "ad80c730a21e5b8d04586a2213dd63b9a0e99e0e2307b0ade35a65485a288648"
        );

        let chunk2_string_to_sign = create_chunk_string_to_sign(
            &date,
            region,
            &chunk1_signature,
            &[Bytes::from(vec![b'a'; 1024])],
        );
        assert_eq!(
            chunk2_string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256-PAYLOAD\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "ad80c730a21e5b8d04586a2213dd63b9a0e99e0e2307b0ade35a65485a288648\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "2edc986847e209b4016e141a6dc8716d3207350f416969382d431539bf292e4a",
            )
        );

        let chunk2_signature =
            calculate_signature(&chunk2_string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            chunk2_signature,
            "0055627c9e194cb4542bae2aa5492e3c1575bbb81b612b7d234b86a503ef5497"
        );

        let chunk3_string_to_sign =
            create_chunk_string_to_sign(&date, region, &chunk2_signature, &[]);
        assert_eq!(
            chunk3_string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256-PAYLOAD\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "0055627c9e194cb4542bae2aa5492e3c1575bbb81b612b7d234b86a503ef5497\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
        );

        let chunk3_signature =
            calculate_signature(&chunk3_string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            chunk3_signature,
            "b6c6ea8a5354eaf15b3cb7646744f4275b71ea724fed81ceb9323e279d449df9"
        );
    }

    #[test]
    fn example_get_bucket_lifecycle_configuration() {
        // let access_key_id = "AKIAIOSFODNN7EXAMPLE";
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let timestamp = "20130524T000000Z";
        // let bucket = "examplebucket";
        let region = "us-east-1";
        let path = "/";

        let headers = OrderedHeaders::from_slice_unchecked(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            (
                "x-amz-content-sha256",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            ("x-amz-date", "20130524T000000Z"),
        ]);

        let query_strings = &[("lifecycle".into(), "".into())];

        let method = Method::GET;

        let canonical_request =
            create_canonical_request(&method, path, query_strings, &headers, Payload::Empty);
        assert_eq!(
            canonical_request,
            concat!(
                "GET\n",
                "/\n",
                "lifecycle=\n",
                "host:examplebucket.s3.amazonaws.com\n",
                "x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "x-amz-date:20130524T000000Z\n",
                "\n",
                "host;x-amz-content-sha256;x-amz-date\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
        );

        let date = AmzDate::from_header_str(timestamp).unwrap();
        let string_to_sign = create_string_to_sign(&canonical_request, &date, region);
        assert_eq!(
            string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "9766c798316ff2757b517bc739a67f6213b4ab36dd5da2f94eaebf79c77395ca",
            )
        );

        let signature = calculate_signature(&string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            signature,
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543"
        );
    }

    #[test]
    fn example_list_objects() {
        // let access_key_id = "AKIAIOSFODNN7EXAMPLE";
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let timestamp = "20130524T000000Z";
        // let bucket = "examplebucket";
        let region = "us-east-1";
        let path = "/";

        let headers = OrderedHeaders::from_slice_unchecked(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            (
                "x-amz-content-sha256",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            ("x-amz-date", "20130524T000000Z"),
        ]);

        let query_strings = &[
            ("max-keys".into(), "2".into()),
            ("prefix".into(), "J".into()),
        ];

        let method = Method::GET;

        let canonical_request =
            create_canonical_request(&method, path, query_strings, &headers, Payload::Empty);

        assert_eq!(
            canonical_request,
            concat!(
                "GET\n",
                "/\n",
                "max-keys=2&prefix=J\n",
                "host:examplebucket.s3.amazonaws.com\n",
                "x-amz-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "x-amz-date:20130524T000000Z\n",
                "\n",
                "host;x-amz-content-sha256;x-amz-date\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
        );

        let date = AmzDate::from_header_str(timestamp).unwrap();
        let string_to_sign = create_string_to_sign(&canonical_request, &date, region);
        assert_eq!(
            string_to_sign,
            concat!(
                "AWS4-HMAC-SHA256\n",
                "20130524T000000Z\n",
                "20130524/us-east-1/s3/aws4_request\n",
                "df57d21db20da04d7fa30298dd4488ba3a2b47ca3a489c74750e0f1e7df1b9b7",
            )
        );

        let signature = calculate_signature(&string_to_sign, secret_access_key, &date, region);
        assert_eq!(
            signature,
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7"
        );
    }
}
from app.security import otp_digest, password_error, valid_email, valid_username


def test_validation_contracts():
    assert valid_username("alice_1")
    assert not valid_username("a")
    assert valid_email("alice@example.com")
    assert not valid_email("alice example.com")
    assert password_error("alice", "alice@example.com", "Weak")
    assert password_error("alice", "alice@example.com", "AlicePassword123!")
    assert otp_digest("k", "A@EXAMPLE.COM", "signup", "123456") == otp_digest("k", "a@example.com", "signup", "123456")


export const isStrongPassword = (password) => {
  if (!password || password.length < 12 || password.length > 128 || /\s/.test(password)) {
    return false;
  }
  const hasUppercase = /[A-Z]/.test(password);
  const hasLowercase = /[a-z]/.test(password);
  const hasDigit = /\d/.test(password);
  const hasSymbol = /[^A-Za-z0-9]/.test(password);
  return hasUppercase && hasLowercase && hasDigit && hasSymbol;
};

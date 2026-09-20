function formatMoneyImpl(amount) {
  return `${amount.toFixed(2)}`;
}

module.exports = { formatMoney: formatMoneyImpl };

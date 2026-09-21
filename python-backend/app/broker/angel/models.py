from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, TypeVar

from pydantic import BaseModel, ConfigDict, Field, SecretStr, field_validator

T = TypeVar("T")


class BrokerReadSuccess[T](BaseModel):
    ok: bool = True
    data: T
    raw: Any = None


class BrokerReadFailure(BaseModel):
    ok: bool = False
    error: Any


BrokerReadResult = BrokerReadSuccess[T] | BrokerReadFailure


@dataclass(frozen=True)
class AccountContext:
    user_id: str
    client_code: str
    api_key: SecretStr
    jwt_token: SecretStr = field(default_factory=lambda: SecretStr(""))
    refresh_token: SecretStr = field(default_factory=lambda: SecretStr(""))
    feed_token: SecretStr = field(default_factory=lambda: SecretStr(""))
    credential_revision: int = 0
    client_local_ip: str = ""
    client_public_ip: str = ""
    client_mac_address: str = ""

    def __repr__(self) -> str:
        return f"AccountContext(user_id={self.user_id!r}, client_code={self.client_code!r}, credential_revision={self.credential_revision})"


@dataclass(frozen=True)
class BrokerSession:
    user_id: str
    client_code: str
    jwt_token: SecretStr
    refresh_token: SecretStr
    feed_token: SecretStr
    received_at: datetime
    credential_revision: int


class RawBrokerModel(BaseModel):
    model_config = ConfigDict(extra="allow")
    raw: dict[str, Any] = Field(default_factory=dict, exclude=True)


class OrderRecord(RawBrokerModel):
    order_id: str | None = Field(default=None, alias="orderid")
    status: str | None = None
    variety: str | None = None
    product_type: str | None = Field(default=None, alias="producttype")
    order_type: str | None = Field(default=None, alias="ordertype")
    transaction_type: str | None = Field(default=None, alias="transactiontype")
    trigger_price: str | None = Field(default=None, alias="triggerprice")
    price: str | None = None
    quantity: str | None = None
    filled_quantity: str | None = Field(default=None, alias="filledshares")
    timestamp: str | None = Field(default=None, alias="updatetime")
    error_code: str | None = Field(default=None, alias="errorcode")
    error_message: str | None = Field(default=None, alias="text")

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> "OrderRecord":
        value = dict(payload)
        value.setdefault("orderid", payload.get("orderId"))
        value["raw"] = dict(payload)
        return cls.model_validate(value)


class TradeRecord(RawBrokerModel):
    order_id: str | None = Field(default=None, alias="orderid")
    trade_id: str | None = Field(default=None, alias="tradeid")
    status: str | None = None
    quantity: str | None = None
    fill_price: str | None = Field(default=None, alias="fillprice")
    timestamp: str | None = Field(default=None, alias="filltime")

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> "TradeRecord":
        value = dict(payload)
        value.setdefault("orderid", payload.get("orderId"))
        value["raw"] = dict(payload)
        return cls.model_validate(value)


class PositionRecord(RawBrokerModel):
    exchange: str | None = None
    symbol: str | None = Field(default=None, alias="tradingsymbol")
    token: str | None = Field(default=None, alias="symboltoken")
    net_quantity: str | None = Field(default=None, alias="netqty")
    average_price: str | None = Field(default=None, alias="netprice")

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> "PositionRecord":
        value = dict(payload)
        value["raw"] = dict(payload)
        return cls.model_validate(value)


class ConditionalRule(RawBrokerModel):
    rule_id: str | None = Field(default=None, alias="id")
    status: str | None = None
    variety: str | None = None
    order_type: str | None = Field(default=None, alias="ordertype")
    trigger_price: str | None = Field(default=None, alias="triggerprice")
    broker_order_id: str | None = Field(default=None, alias="orderid")

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> "ConditionalRule":
        value = dict(payload)
        value["raw"] = dict(payload)
        return cls.model_validate(value)


class MarketTick(BaseModel):
    type: str = "tick"
    subscription_mode: int
    exchange_type: int
    token: str
    sequence_number: int
    exchange_timestamp: int
    last_traded_price: float
    last_traded_quantity: int | None = None
    average_traded_price: float | None = None
    volume_trade_for_the_day: int | None = None
    open_price_of_the_day: float | None = None
    high_price_of_the_day: float | None = None
    low_price_of_the_day: float | None = None
    closed_price: float | None = None


class OrderMutationRequest(BaseModel):
    model_config = ConfigDict(frozen=True)

    variety: str = "NORMAL"
    trading_symbol: str
    symbol_token: str
    transaction_type: str
    exchange: str
    order_type: str
    product_type: str
    duration: str = "DAY"
    price: str = "0"
    square_off: str = "0"
    stop_loss: str = "0"
    quantity: int
    trigger_price: str = "0"
    client_reference: str

    @field_validator("transaction_type")
    @classmethod
    def valid_side(cls, value: str) -> str:
        normalized = value.upper()
        if normalized not in {"BUY", "SELL"}:
            raise ValueError("transaction_type must be BUY or SELL")
        return normalized

    @field_validator("quantity")
    @classmethod
    def positive_quantity(cls, value: int) -> int:
        if value <= 0:
            raise ValueError("quantity must be positive")
        return value

    @field_validator(
        "variety",
        "trading_symbol",
        "symbol_token",
        "exchange",
        "order_type",
        "product_type",
        "duration",
        "client_reference",
    )
    @classmethod
    def nonempty(cls, value: str) -> str:
        normalized = value.strip()
        if not normalized:
            raise ValueError("broker order fields must not be empty")
        return normalized

    def angel_payload(self) -> dict[str, str]:
        return {
            "variety": self.variety,
            "tradingsymbol": self.trading_symbol,
            "symboltoken": self.symbol_token,
            "transactiontype": self.transaction_type,
            "exchange": self.exchange,
            "ordertype": self.order_type,
            "producttype": self.product_type,
            "duration": self.duration,
            "price": self.price,
            "squareoff": self.square_off,
            "stoploss": self.stop_loss,
            "quantity": str(self.quantity),
            "triggerprice": self.trigger_price,
            "ordertag": self.client_reference,
        }


class CancelOrderRequest(BaseModel):
    model_config = ConfigDict(frozen=True)
    variety: str
    order_id: str

    @field_validator("variety", "order_id")
    @classmethod
    def nonempty(cls, value: str) -> str:
        normalized = value.strip()
        if not normalized:
            raise ValueError("cancel fields must not be empty")
        return normalized

    def angel_payload(self) -> dict[str, str]:
        return {"variety": self.variety, "orderid": self.order_id}


class ModifyOrderRequest(OrderMutationRequest):
    order_id: str

    def angel_payload(self) -> dict[str, str]:
        return {**super().angel_payload(), "orderid": self.order_id}


class ConditionalMutationRequest(BaseModel):
    model_config = ConfigDict(frozen=True)
    trading_symbol: str
    symbol_token: str
    exchange: str
    product_type: str
    transaction_type: str
    price: str
    quantity: int
    trigger_price: str
    rule_id: str | None = None

    @field_validator("quantity")
    @classmethod
    def positive_quantity(cls, value: int) -> int:
        if value <= 0:
            raise ValueError("quantity must be positive")
        return value

    def angel_payload(self) -> dict[str, str]:
        payload = {
            "tradingsymbol": self.trading_symbol,
            "symboltoken": self.symbol_token,
            "exchange": self.exchange,
            "producttype": self.product_type,
            "transactiontype": self.transaction_type,
            "price": self.price,
            "qty": str(self.quantity),
            "triggerprice": self.trigger_price,
            "timeperiod": "365",
        }
        if self.rule_id:
            payload["id"] = self.rule_id
        return payload


class BrokerMutationResponse(BaseModel):
    operation: str
    broker_order_id: str
    unique_order_id: str = ""
    raw: Any = Field(default=None, exclude=True)


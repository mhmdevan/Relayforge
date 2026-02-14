use anyhow::Context;
use futures_util::StreamExt;
use lapin::{
    options::*,
    types::{AMQPValue, FieldTable, LongString},
    BasicProperties, Channel, Connection, ConnectionProperties, ExchangeKind,
};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct RabbitConfig {
    pub addr: String,
    pub exchange: String,
    pub dlx: String,
    pub queue: String,
    pub retry_queue: String,
    pub dlq: String,
    pub prefetch: u16,
    pub retry_ttl_ms: u32,
}

pub struct Rabbit {
    pub conn: Connection,
    pub ch: Channel,
    pub cfg: RabbitConfig,
}

impl Rabbit {
    pub async fn connect(cfg: RabbitConfig) -> anyhow::Result<Self> {
        Self::connect_publisher(cfg).await
    }

    pub async fn connect_publisher(cfg: RabbitConfig) -> anyhow::Result<Self> {
        Self::connect_inner(cfg, true).await
    }

    pub async fn connect_consumer(cfg: RabbitConfig) -> anyhow::Result<Self> {
        Self::connect_inner(cfg, false).await
    }

    async fn connect_inner(cfg: RabbitConfig, enable_confirms: bool) -> anyhow::Result<Self> {
        let props =
            ConnectionProperties::default().with_connection_name("relayforge-worker".into());

        let conn = Connection::connect(&cfg.addr, props)
            .await
            .context("failed to connect to rabbitmq")?;

        let ch = conn
            .create_channel()
            .await
            .context("create_channel failed")?;

        ch.basic_qos(cfg.prefetch, BasicQosOptions { global: false })
            .await
            .context("basic_qos failed")?;

        if enable_confirms {
            ch.confirm_select(ConfirmSelectOptions::default())
                .await
                .context("confirm_select failed")?;
        }

        let rabbit = Self { conn, ch, cfg };
        rabbit.setup_topology().await?;
        Ok(rabbit)
    }

    async fn setup_topology(&self) -> anyhow::Result<()> {
        self.ch
            .exchange_declare(
                &self.cfg.exchange,
                ExchangeKind::Topic,
                ExchangeDeclareOptions {
                    durable: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await
            .context("exchange_declare events failed")?;

        // DLX
        self.ch
            .exchange_declare(
                &self.cfg.dlx,
                ExchangeKind::Direct,
                ExchangeDeclareOptions {
                    durable: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await
            .context("exchange_declare dlx failed")?;

        // DLQ
        self.ch
            .queue_declare(
                &self.cfg.dlq,
                QueueDeclareOptions {
                    durable: true,
                    ..Default::default()
                },
                FieldTable::default(),
            )
            .await
            .context("queue_declare dlq failed")?;

        self.ch
            .queue_bind(
                &self.cfg.dlq,
                &self.cfg.dlx,
                "relayforge.jobs.dlq",
                QueueBindOptions::default(),
                FieldTable::default(),
            )
            .await
            .context("queue_bind dlq failed")?;

        // Main queue with dead-letter to DLX
        let mut main_args = FieldTable::default();
        main_args.insert(
            "x-dead-letter-exchange".into(),
            AMQPValue::LongString(LongString::from(self.cfg.dlx.clone())),
        );
        main_args.insert(
            "x-dead-letter-routing-key".into(),
            AMQPValue::LongString(LongString::from("relayforge.jobs.dlq")),
        );

        self.ch
            .queue_declare(
                &self.cfg.queue,
                QueueDeclareOptions {
                    durable: true,
                    ..Default::default()
                },
                main_args,
            )
            .await
            .context("queue_declare main failed")?;

        self.ch
            .queue_bind(
                &self.cfg.queue,
                &self.cfg.exchange,
                "job.created",
                QueueBindOptions::default(),
                FieldTable::default(),
            )
            .await
            .context("queue_bind main failed")?;

        // Retry queue: TTL then DL back to exchange/job.created
        let mut retry_args = FieldTable::default();
        retry_args.insert(
            "x-message-ttl".into(),
            AMQPValue::LongUInt(self.cfg.retry_ttl_ms),
        );
        retry_args.insert(
            "x-dead-letter-exchange".into(),
            AMQPValue::LongString(LongString::from(self.cfg.exchange.clone())),
        );
        retry_args.insert(
            "x-dead-letter-routing-key".into(),
            AMQPValue::LongString(LongString::from("job.created")),
        );

        self.ch
            .queue_declare(
                &self.cfg.retry_queue,
                QueueDeclareOptions {
                    durable: true,
                    ..Default::default()
                },
                retry_args,
            )
            .await
            .context("queue_declare retry failed")?;

        self.ch
            .queue_bind(
                &self.cfg.retry_queue,
                &self.cfg.exchange,
                "job.retry",
                QueueBindOptions::default(),
                FieldTable::default(),
            )
            .await
            .context("queue_bind retry failed")?;

        info!(
            exchange = %self.cfg.exchange,
            queue = %self.cfg.queue,
            retry_queue = %self.cfg.retry_queue,
            dlq = %self.cfg.dlq,
            "rabbitmq topology ready"
        );

        Ok(())
    }

    pub async fn publish(&self, routing_key: &str, payload: &[u8]) -> anyhow::Result<()> {
        let confirm = self
            .ch
            .basic_publish(
                &self.cfg.exchange,
                routing_key,
                BasicPublishOptions::default(),
                payload,
                BasicProperties::default()
                    .with_content_type("application/json".into())
                    .with_delivery_mode(2),
            )
            .await
            .context("basic_publish failed")?
            .await
            .context("publish confirm await failed")?;

        if confirm.is_nack() {
            anyhow::bail!("rabbitmq publish got NACK");
        }
        Ok(())
    }

    pub async fn consume_loop<F, Fut>(
        &self,
        consumer_tag: &str,
        max_concurrency: usize,
        handler: F,
    ) -> anyhow::Result<()>
    where
        F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<ConsumeAction>> + Send + 'static,
    {
        let consumer = self
            .ch
            .basic_consume(
                &self.cfg.queue,
                consumer_tag,
                BasicConsumeOptions::default(),
                FieldTable::default(),
            )
            .await
            .context("basic_consume failed")?;

        info!("consumer started");
        let handler = Arc::new(handler);
        let max_concurrency = max_concurrency.max(1);

        consumer
            .for_each_concurrent(max_concurrency, |delivery| {
                let handler = Arc::clone(&handler);
                async move {
                    let delivery = match delivery {
                        Ok(d) => d,
                        Err(e) => {
                            warn!(error = %e, "consumer delivery error");
                            return;
                        }
                    };

                    let payload = delivery.data.clone();

                    match handler(payload).await {
                        Ok(ConsumeAction::Ack) => {
                            delivery.ack(BasicAckOptions::default()).await.ok();
                        }
                        Ok(ConsumeAction::Retry) => {
                            delivery.ack(BasicAckOptions::default()).await.ok();
                        }
                        Ok(ConsumeAction::DeadLetter) => {
                            delivery
                                .nack(BasicNackOptions {
                                    requeue: false,
                                    multiple: false,
                                })
                                .await
                                .ok();
                        }
                        Err(e) => {
                            warn!(error = %e, "handler error => deadletter");
                            delivery
                                .nack(BasicNackOptions {
                                    requeue: false,
                                    multiple: false,
                                })
                                .await
                                .ok();
                        }
                    }
                }
            })
            .await;

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConsumeAction {
    Ack,
    Retry,
    DeadLetter,
}
